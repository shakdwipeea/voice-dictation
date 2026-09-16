#!/usr/bin/env python3
"""Run safe System Mode verification modes and emit JSON/Markdown/HTML evidence."""

from __future__ import annotations

import argparse
import datetime as dt
import html
import json
import os
import platform
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Callable


ROOT = Path(__file__).resolve().parents[4]
SYSTEM_RUST_FILES = [
    "apps/daemon/src/daemon.rs",
    "apps/daemon/src/settings.rs",
    "apps/daemon/src/system_mode.rs",
    "apps/daemon/src/system_worker.rs",
    "crates/sunoto-linux/src/system/linux.rs",
    "crates/sunoto-linux/src/system/stub.rs",
    "crates/sunoto-macos/src/system.rs",
] + [str(path.relative_to(ROOT)) for path in sorted((ROOT / "crates/sunoto-system/src").glob("*.rs"))]


@dataclass
class Check:
    mode: str
    name: str
    status: str
    duration_ms: int
    command: list[str]
    summary: str
    log: str | None = None


class Harness:
    def __init__(self, output: Path, modes: list[str], inject_failure: bool) -> None:
        self.output = output
        self.logs = output / "logs"
        self.logs.mkdir(parents=True, exist_ok=False)
        self.modes = modes
        self.inject_failure = inject_failure
        self.checks: list[Check] = []
        self.live_actions: list[str] = []
        self.cleanup: list[str] = []

    def redact(self, text: str) -> str:
        text = text.replace(str(ROOT), "$REPO")
        home = str(Path.home())
        text = text.replace(home, "$HOME")
        text = re.sub(r"(?:target|session)-\d+", "<opaque-id>", text)
        return text

    def add(
        self,
        mode: str,
        name: str,
        command: list[str],
        timeout: int = 300,
        env: dict[str, str] | None = None,
        evaluate: Callable[[subprocess.CompletedProcess[str]], tuple[bool, str]] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        started = time.monotonic()
        complete = subprocess.run(
            command,
            cwd=ROOT,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=timeout,
            check=False,
        )
        passed, summary = (
            evaluate(complete)
            if evaluate
            else (complete.returncode == 0, f"exit code {complete.returncode}")
        )
        safe_name = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")
        log_path = self.logs / f"{mode}-{safe_name}.log"
        log_path.write_text(self.redact(complete.stdout), encoding="utf-8")
        self.checks.append(
            Check(
                mode=mode,
                name=name,
                status="PASS" if passed else "FAIL",
                duration_ms=int((time.monotonic() - started) * 1000),
                command=[self.redact(part) for part in command],
                summary=self.redact(summary),
                log=str(log_path.relative_to(self.output)),
            )
        )
        return complete

    def skip(self, mode: str, name: str, reason: str) -> None:
        self.checks.append(Check(mode, name, "SKIP", 0, [], reason))

    def static(self) -> None:
        mode = "static"
        self.add(mode, "workspace build", ["cargo", "build", "--workspace", "--offline"], 600)
        self.add(
            mode,
            "System Mode formatting",
            ["rustfmt", "--edition", "2024", "--check", *SYSTEM_RUST_FILES],
        )
        self.add(mode, "workspace Rust tests", ["cargo", "test", "--workspace", "--offline"], 900)
        self.add(
            mode,
            "workspace clippy",
            ["cargo", "clippy", "--workspace", "--offline", "--all-targets", "--", "-D", "warnings"],
            900,
        )
        for suite in ("phase0", "phase1", "phase2", "ui"):
            self.add(
                mode,
                f"Python {suite}",
                [sys.executable, "-m", "unittest", "discover", "-s", f"tests/{suite}", "-v"],
                300,
            )

    @staticmethod
    def json_eval(predicate: Callable[[dict[str, object]], bool], label: str):
        def evaluate(complete: subprocess.CompletedProcess[str]) -> tuple[bool, str]:
            try:
                payload = json.loads(complete.stdout)
            except json.JSONDecodeError as error:
                return False, f"invalid JSON: {error}"
            return complete.returncode == 0 and predicate(payload), label

        return evaluate

    def contract(self) -> None:
        mode = "contract"
        self.add(mode, "typed System contracts", ["cargo", "test", "-p", "sunoto-system", "--offline"], 300)
        libraries = list((ROOT / "target/debug/deps").glob("libsunoto_system-*.rlib"))
        if not libraries:
            self.checks.append(
                Check(
                    mode,
                    "Linux provider contract compilation",
                    "FAIL",
                    0,
                    [],
                    "sunoto-system library artifact was not produced",
                )
            )
        else:
            library = max(libraries, key=lambda path: path.stat().st_mtime_ns)
            binary = self.output / ".linux-provider-contract-tests"
            try:
                compiled = self.add(
                    mode,
                    "Linux provider contract compilation",
                    [
                        "rustc",
                        "--edition",
                        "2024",
                        "--test",
                        "crates/sunoto-linux/src/system/linux.rs",
                        "--extern",
                        f"sunoto_system={library}",
                        "-L",
                        "dependency=target/debug/deps",
                        "-o",
                        str(binary),
                    ],
                )
                if compiled.returncode == 0:
                    self.add(mode, "Linux provider non-live contracts", [str(binary)])
            finally:
                binary.unlink(missing_ok=True)
            self.cleanup.append(
                f"Linux provider contract binary: {'clean' if not binary.exists() else 'not clean'}"
            )
        self.add(
            mode,
            "daemon fixture integration",
            ["cargo", "test", "-p", "sunoto-daemon", "system_mode_integration_tests", "--offline"],
            300,
        )
        self.add(
            mode,
            "macOS provider non-live contracts",
            ["cargo", "test", "-p", "sunoto-macos", "system::tests::", "--offline"],
            300,
        )
        self.add(mode, "daemon build for dry-run CLI", ["cargo", "build", "-p", "sunoto-daemon", "--offline"], 300)
        daemon = str(ROOT / "target/debug/sunoto-daemon")
        self.add(
            mode,
            "project and editor dry-run",
            [daemon, "system", "plan", "open who-else-is-free in VS Code"],
            evaluate=self.json_eval(
                lambda value: value.get("execution_allowed") is False
                and value.get("steps", [{}])[0].get("capability") == "target.find"
                and "/Users/" not in json.dumps(value),
                "typed target.find; execution disabled; no native path",
            ),
        )
        self.add(
            mode,
            "real project read-only resolve",
            [daemon, "system", "resolve", "open who-else-is-free"],
            evaluate=self.json_eval(
                lambda value: value.get("execution_allowed") is False
                and any(
                    suggestion.get("title") == "Open who-else-is-free"
                    for suggestion in value.get("suggestions", [])
                ),
                "real project present; selection required",
            ),
        )
        self.add(
            mode,
            "adversarial route rejection",
            [daemon, "system", "plan", "open Chrome; rm -rf harmless-fixture"],
            evaluate=self.json_eval(
                lambda value: value.get("route", {}).get("status") == "rejected"
                and value.get("steps") == []
                and value.get("execution_allowed") is False,
                "unsafe multi-action rejected before resolution",
            ),
        )
        self.add(
            mode,
            "unified no-match",
            [daemon, "system", "resolve", "open sunoto-definitely-missing-target-9284"],
            evaluate=self.json_eval(
                lambda value: value.get("suggestions") == []
                and value.get("next_step") == "none"
                and value.get("execution_allowed") is False,
                "no match; no application-only fallback; no execution",
            ),
        )
        if self.inject_failure:
            self.add(
                mode,
                "injected negative fixture",
                [sys.executable, "-c", "raise SystemExit(23)"],
                evaluate=lambda complete: (
                    complete.returncode == 0,
                    "intentional reporter-integrity fixture must be reported as FAIL",
                ),
            )

    @staticmethod
    def running_daemons() -> list[str]:
        complete = subprocess.run(
            ["ps", "ax", "-o", "pid=,command="],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        return [
            line.strip()
            for line in complete.stdout.splitlines()
            if "sunoto-daemon" in line and " run" in line and "run_e2e.py" not in line
        ]

    @staticmethod
    def send_control(path: Path, payload: dict[str, object]) -> str:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(5)
            client.connect(str(path))
            client.sendall(json.dumps(payload).encode("utf-8") + b"\n")
            if payload.get("type") == "plan_system":
                chunks = []
                while True:
                    block = client.recv(65536)
                    if not block:
                        break
                    chunks.append(block)
                return b"".join(chunks).decode("utf-8")
        return ""

    def daemon_product_path(self) -> None:
        mode = "daemon"
        started = time.monotonic()
        if self.running_daemons():
            self.checks.append(
                Check(mode, "mock daemon product path", "FAIL", 0, [], "an existing daemon is running; refused to start a second")
            )
            return
        subprocess.run(
            ["cargo", "build", "-p", "sunoto-daemon", "--offline"],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
        temp_path: Path | None = None
        passed = False
        summary = "mock daemon did not reach the palette"
        preserved_log = self.logs / "daemon-mock-product-path.log"
        command = ["target/debug/sunoto-daemon", "run", "--config", "<temporary-config>"]
        try:
            with tempfile.TemporaryDirectory(prefix="sunoto-system-e2e-") as directory:
                temp_path = Path(directory)
                fixture = temp_path / "approved/e2e-fixture-project"
                (fixture / ".git").mkdir(parents=True)
                (fixture / "README.md").write_text("harmless E2E fixture\n", encoding="utf-8")
                config = temp_path / "config.json"
                config.write_text(
                    json.dumps(
                        {
                            "backend": "mock",
                            "system_mode_enabled": True,
                            "system_search_roots": [str(temp_path / "approved")],
                            "overlay_enabled": True,
                            "overlay_backend": "mock",
                            "polish_enabled": False,
                            "llm_polish_enabled": False,
                        }
                    ),
                    encoding="utf-8",
                )
                control = temp_path / "control.sock"
                overlay_log = temp_path / "overlay.ndjson"
                daemon_log = temp_path / "daemon.log"
                environment = os.environ.copy()
                environment.update(
                    {
                        "SUNOTO_CONTROL_SOCKET": str(control),
                        "SUNOTO_MOCK_FINAL_TEXT": "open e2e-fixture-project",
                        "SUNOTO_MOCK_OVERLAY_LOG": str(overlay_log),
                    }
                )
                with daemon_log.open("w", encoding="utf-8") as log_handle:
                    process = subprocess.Popen(
                        [str(ROOT / "target/debug/sunoto-daemon"), "run", "--config", str(config)],
                        cwd=ROOT,
                        env=environment,
                        text=True,
                        stdout=log_handle,
                        stderr=subprocess.STDOUT,
                    )
                    try:
                        deadline = time.monotonic() + 15
                        while time.monotonic() < deadline:
                            log_text = daemon_log.read_text(encoding="utf-8", errors="replace")
                            if control.exists() and "ASR sidecar ready" in log_text and "overlay UI ready" in log_text:
                                break
                            if process.poll() is not None:
                                raise RuntimeError(f"daemon exited early with {process.returncode}")
                            time.sleep(0.1)
                        else:
                            raise RuntimeError("daemon readiness deadline exceeded")
                        self.send_control(control, {"type": "trigger", "mode": "system", "edge": "press"})
                        time.sleep(0.35)
                        self.send_control(control, {"type": "trigger", "mode": "system", "edge": "release"})
                        deadline = time.monotonic() + 15
                        palette = None
                        while time.monotonic() < deadline:
                            if overlay_log.exists():
                                events = [json.loads(line) for line in overlay_log.read_text().splitlines() if line]
                                palette = next((event for event in events if event.get("type") == "system_palette"), None)
                            log_text = daemon_log.read_text(encoding="utf-8", errors="replace")
                            if palette and "System suggestions dismissed" in log_text:
                                break
                            time.sleep(0.1)
                        plan = json.loads(
                            self.send_control(
                                control,
                                {"type": "plan_system", "text": "open e2e-fixture-project", "dry_run": True},
                            )
                        )
                        titles = [item.get("title") for item in (palette or {}).get("suggestions", [])]
                        passed = (
                            "Open e2e-fixture-project" in titles
                            and plan.get("execution_allowed") is False
                            and plan.get("steps", [{}])[0].get("capability") == "target.find"
                        )
                        summary = "mock ASR → worker target.find → headless palette → cancellation; dry-run control response verified"
                    finally:
                        if process.poll() is None:
                            process.send_signal(signal.SIGTERM)
                            try:
                                process.wait(timeout=10)
                            except subprocess.TimeoutExpired:
                                process.kill()
                                process.wait(timeout=5)
                preserved_log.write_text(
                    self.redact(daemon_log.read_text(encoding="utf-8", errors="replace")),
                    encoding="utf-8",
                )
                passed = passed and not control.exists() and process.poll() is not None
        except Exception as error:  # report the evidence instead of crashing the whole run
            summary = f"{type(error).__name__}: {error}"
        cleanup_ok = temp_path is not None and not temp_path.exists() and not self.running_daemons()
        self.cleanup.append(f"mock daemon fixtures/process/socket: {'clean' if cleanup_ok else 'not clean'}")
        passed = passed and cleanup_ok
        self.checks.append(
            Check(
                mode,
                "mock daemon product path",
                "PASS" if passed else "FAIL",
                int((time.monotonic() - started) * 1000),
                command,
                self.redact(summary),
                str(preserved_log.relative_to(self.output)) if preserved_log.exists() else None,
            )
        )

    def daemon(self) -> None:
        mode = "daemon"
        self.add(
            mode,
            "normal dictation state regression",
            ["cargo", "test", "-p", "sunoto-core", "--offline"],
            300,
        )
        self.add(
            mode,
            "fixture transcript to observation",
            ["cargo", "test", "-p", "sunoto-daemon", "system_mode_integration_tests", "--offline"],
            300,
        )
        self.daemon_product_path()

    def live_macos(self, confirmation: str | None) -> None:
        mode = "live-macos"
        if platform.system() != "Darwin":
            self.skip(mode, "macOS native actions", f"host is {platform.system()}, not macOS")
            return
        if confirmation != "I confirm live-macos":
            self.skip(mode, "macOS native actions", "missing immediate exact confirmation: I confirm live-macos")
            return
        commands = [
            ("default and selected browser", "system::tests::live_default_and_selected_chrome_navigation"),
            ("disposable file folder reveal", "system::tests::live_disposable_file_folder_and_reveal"),
            ("real project in selected editor", "system::tests::live_project_in_selected_editor"),
        ]
        for name, test in commands:
            complete = self.add(
                mode,
                name,
                ["cargo", "test", "-p", "sunoto-macos", test, "--offline", "--", "--ignored", "--exact", "--nocapture"],
                300,
            )
            if complete.returncode == 0:
                self.live_actions.append(name)

    def live_linux(self, confirmation: str | None) -> None:
        mode = "live-linux"
        if platform.system() != "Linux":
            self.skip(mode, "Linux native actions", f"host is {platform.system()}, not Linux")
            return
        if confirmation != "I confirm live-linux":
            self.skip(mode, "Linux native actions", "missing immediate exact confirmation: I confirm live-linux")
            return
        complete = self.add(
            mode,
            "GIO live contracts",
            ["cargo", "test", "-p", "sunoto-linux", "system::linux::tests::live_", "--offline", "--", "--ignored", "--nocapture"],
            300,
        )
        if complete.returncode == 0:
            self.live_actions.append("Linux GIO application, URL, and disposable local targets")

    def write_reports(self) -> None:
        counts = {status: sum(check.status == status for check in self.checks) for status in ("PASS", "FAIL", "SKIP")}
        artifact = {
            "schema_version": 1,
            "type": "sunoto_system_mode_e2e",
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "host": {
                "platform": platform.system(),
                "release": platform.release(),
                "machine": platform.machine(),
                "python": platform.python_version(),
            },
            "modes": self.modes,
            "counts": counts,
            "checks": [asdict(check) for check in self.checks],
            "covered_phase_criteria": [
                "typed routing/planning/policy",
                "unified target providers/ranking/store",
                "explicit selection and one-time authorization",
                "cancellation/deadline/expiry/mutation/adversarial safety",
                "mock daemon transcript-to-palette path",
                "normal dictation regression",
            ],
            "live_actions_observed": self.live_actions,
            "cleanup": self.cleanup,
            "redaction": "home/repository paths and opaque target/session IDs redacted from captured logs",
        }
        (self.output / "artifact.json").write_text(json.dumps(artifact, indent=2) + "\n", encoding="utf-8")
        lines = [
            "# Sunoto System Mode E2E",
            "",
            f"Result: **{counts['PASS']} passed, {counts['FAIL']} failed, {counts['SKIP']} skipped**",
            "",
            f"Host: {artifact['host']['platform']} {artifact['host']['release']} ({artifact['host']['machine']})",
            "",
            "| Mode | Check | Status | Time | Evidence |",
            "| --- | --- | --- | ---: | --- |",
        ]
        for check in self.checks:
            lines.append(f"| {check.mode} | {check.name} | {check.status} | {check.duration_ms}ms | {check.summary} |")
        lines += ["", "## Live actions", "", *(f"- {item}" for item in self.live_actions or ["None (non-live run)"]), "", "## Cleanup", "", *(f"- {item}" for item in self.cleanup or ["No temporary daemon resources created"])]
        (self.output / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
        rows = "".join(
            f"<tr><td>{html.escape(check.mode)}</td><td>{html.escape(check.name)}</td>"
            f"<td><span class='{check.status.lower()}'>{check.status}</span></td>"
            f"<td>{check.duration_ms} ms</td><td>{html.escape(check.summary)}</td></tr>"
            for check in self.checks
        )
        report = f"""<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'><title>Sunoto System Mode E2E</title><style>
body{{font:15px/1.5 Inter,system-ui,sans-serif;background:#0b1020;color:#e8ecf8;margin:0}}main{{max-width:1180px;margin:auto;padding:44px 28px}}h1{{font-size:34px;margin:0 0 8px}}.meta{{color:#9da9c7}}.cards{{display:grid;grid-template-columns:repeat(3,1fr);gap:14px;margin:28px 0}}.card{{background:#141b31;border:1px solid #283453;border-radius:14px;padding:20px}}.number{{font-size:32px;font-weight:750}}table{{width:100%;border-collapse:collapse;background:#11182b;border-radius:14px;overflow:hidden}}th,td{{padding:13px 14px;border-bottom:1px solid #26314d;text-align:left;vertical-align:top}}th{{color:#aeb9d5;background:#17203a}}.pass{{color:#67e8a3}}.fail{{color:#ff7a90}}.skip{{color:#f4c96b}}code{{color:#b8c7ff}}section{{margin-top:30px}}@media(max-width:760px){{.cards{{grid-template-columns:1fr}}table{{font-size:12px}}}}
</style></head><body><main><h1>Sunoto System Mode E2E</h1><p class='meta'>{html.escape(artifact['generated_at'])} · {html.escape(artifact['host']['platform'])} {html.escape(artifact['host']['release'])} · modes: {html.escape(', '.join(self.modes))}</p><div class='cards'><div class='card'><div class='number pass'>{counts['PASS']}</div>passed</div><div class='card'><div class='number fail'>{counts['FAIL']}</div>failed</div><div class='card'><div class='number skip'>{counts['SKIP']}</div>skipped</div></div><section><h2>Checks</h2><table><thead><tr><th>Mode</th><th>Check</th><th>Status</th><th>Time</th><th>Evidence</th></tr></thead><tbody>{rows}</tbody></table></section><section><h2>Live actions observed</h2><p>{html.escape('; '.join(self.live_actions) if self.live_actions else 'None — this was a non-live run.')}</p></section><section><h2>Cleanup</h2><p>{html.escape('; '.join(self.cleanup) if self.cleanup else 'No temporary daemon resources created.')}</p></section><section><h2>Artifacts</h2><p><code>artifact.json</code>, <code>report.md</code>, and redacted <code>logs/</code></p></section></main></body></html>"""
        (self.output / "report.html").write_text(report, encoding="utf-8")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", action="append", choices=("static", "contract", "daemon", "live-macos", "live-linux"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--confirm-live")
    parser.add_argument("--inject-failure", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    modes = args.mode or ["static", "contract", "daemon"]
    output = args.output.resolve()
    if output.exists():
        raise SystemExit(f"refusing to overwrite existing output: {output}")
    output.mkdir(parents=True)
    harness = Harness(output, modes, args.inject_failure)
    for mode in modes:
        getattr(harness, mode.replace("-", "_"))(args.confirm_live) if mode.startswith("live-") else getattr(harness, mode)()
    harness.write_reports()
    counts = {status: sum(check.status == status for check in harness.checks) for status in ("PASS", "FAIL", "SKIP")}
    print(json.dumps({"output": str(output), "counts": counts}, indent=2))
    return 1 if counts["FAIL"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
