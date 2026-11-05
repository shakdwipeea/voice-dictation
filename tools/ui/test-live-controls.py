#!/usr/bin/env python3
"""Non-transcribing checks against the sole installed daemon; leaves it resumed."""
import json, os, socket, tempfile, time
from pathlib import Path

socket_path = Path(tempfile.gettempdir()) / ("sunoto-" + os.environ["USER"] + "-daemon.sock")
results = []
def command(payload):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(10)
        client.connect(str(socket_path))
        client.sendall(json.dumps(payload).encode() + b"\n")
        data = b""
        while not data.endswith(b"\n"):
            block = client.recv(65536)
            if not block: break
            data += block
        return json.loads(data)
def status(): return command({"type":"status"})
def action(name): return command({"type":"user_control","action":name,"confirmed":True})
def wait_for(predicate, seconds=5):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        state = status()
        if predicate(state): return state
        time.sleep(.1)
    raise AssertionError(state)
def record(name, state):
    results.append({"check":name,"passed":True,"state":state})
    print("PASS:", name, flush=True)

history = Path.home() / "Library/Application Support/sunoto/history"
def history_snapshot():
    return {str(p.relative_to(history)): (p.stat().st_size, p.stat().st_mtime_ns)
            for p in history.rglob("*") if p.is_file()} if history.exists() else {}
before_history = history_snapshot()
try:
    state = wait_for(lambda s: s["ready"])
    record("Ready after installed restart", state)
    action("open_settings")
    time.sleep(.5)
    record("Opening Settings leaves microphone idle", wait_for(lambda s: s["microphone"] == "idle" and not s["preview"]))
    action("preview_start")
    record("Explicit test starts the daemon microphone", wait_for(lambda s: s["preview"] and s["microphone"] == "capturing"))
    time.sleep(1)
    record("Test remains outside dictation session", wait_for(lambda s: s["session"] == "idle" and s["preview"]))
    action("settings_closed")
    record("Closing Settings stops test and microphone", wait_for(lambda s: not s["preview"] and s["microphone"] == "idle"))
    action("preview_start")
    wait_for(lambda s: s["preview"] and s["microphone"] == "capturing")
    action("pause")
    record("Pause stops microphone test", wait_for(lambda s: s["paused"] and not s["preview"] and s["microphone"] == "idle" and not s["ready"]))
    action("preview_start")
    time.sleep(.3)
    record("Paused app refuses microphone test", wait_for(lambda s: s["paused"] and not s["preview"] and s["microphone"] == "idle"))
    action("resume")
    record("Resume restores ready without opening microphone", wait_for(lambda s: s["ready"] and s["microphone"] == "idle"))
    action("preview_start")
    wait_for(lambda s: s["preview"])
    record("Microphone test automatically expires", wait_for(lambda s: not s["preview"] and s["microphone"] == "idle", 35))
    assert history_snapshot() == before_history, "Preview changed history"
    record("Preview tests create no history files or entries", status())
    action("practice_start")
    wait_for(lambda s: s["practice"])
    action("pause")
    record("Pause keeps the practice destination", wait_for(lambda s: s["paused"] and s["practice"]))
    action("resume")
    record("Resume stays in practice", wait_for(lambda s: not s["paused"] and s["practice"]))
    action("interrupt")
    time.sleep(.3)
    record("Sleep keeps the practice destination", wait_for(lambda s: s["practice"] and s["session"] == "idle"))
    action("practice_stop")
    record("Closing practice ends it", wait_for(lambda s: not s["practice"]))
finally:
    Path("docs/evidence/macos-user-controls/live-controls.json").write_text(json.dumps(results, indent=2) + "\n")
    action("preview_stop")
    action("practice_stop")
    action("resume")
