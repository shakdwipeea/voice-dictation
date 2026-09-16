"""stdin-driven UI sidecar entry point.

The Rust daemon spawns this process (managed by sunoto-ipc like the ASR
sidecar) and streams newline-delimited JSON ops on stdin, using the same
"type"-tagged protocol shape as the ASR sidecar:

    {"type": "show"} / {"type": "hide"}
    {"type": "recording", "elapsed_s": 1.2, "peak": 0.4, "rms": 0.05, "segments": 2}
    {"type": "status", "text": "transcribing"}
    {"type": "state", "name": "loading_asr", "detail": "loading speech model"}
    {"type": "segment", "text": "..."} / {"type": "clear"}
    {"type": "system_palette", "session_id": 1, ...}
    {"type": "dismiss_system_palette", "session_id": 1}
    {"type": "shutdown"}

The sidecar emits `ready` and System palette selection/cancellation events on
stdout. stdin EOF is equivalent to shutdown, so an exiting daemon always takes
the overlay down with it.

GTK is imported only in main() — dispatch() stays importable (and testable)
on machines without GTK4.
"""
from __future__ import annotations

import json
import logging
import sys
import threading

log = logging.getLogger(__name__)


def dispatch(overlay, msg: dict) -> bool:
    """Apply one decoded message to the overlay.

    Returns False when the sidecar should shut down, True otherwise.
    Unknown ops and missing fields are logged and skipped, never fatal —
    a malformed UI frame must not kill the overlay mid-dictation.
    """
    op = msg.get("type")
    try:
        if op == "show":
            overlay.show()
        elif op == "hide":
            overlay.hide()
        elif op == "recording":
            overlay.set_recording(
                float(msg.get("elapsed_s", 0.0)),
                float(msg.get("peak", 0.0)),
                float(msg.get("rms", 0.0)),
                int(msg.get("segments", 0)),
            )
        elif op == "status":
            overlay.set_status(str(msg.get("text", "")))
        elif op == "state":
            overlay.set_state(str(msg.get("name", "")), str(msg.get("detail", "")))
        elif op == "segment":
            overlay.add_segment(str(msg.get("text", "")))
        elif op == "clear":
            overlay.clear_segments()
        elif op == "system_palette":
            overlay.show_system_palette(
                int(msg["session_id"]),
                str(msg.get("transcript", "")),
                list(msg.get("suggestions", [])),
            )
        elif op == "dismiss_system_palette":
            overlay.dismiss_system_palette(int(msg["session_id"]))
        elif op == "shutdown":
            return False
        else:
            log.warning("unknown op: %r", op)
    except (TypeError, ValueError):
        log.warning("bad fields in %r", msg)
    return True


def _pump_stdin(overlay) -> None:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            log.warning("bad JSON line: %r", line)
            continue
        if not dispatch(overlay, msg):
            break
    overlay.shutdown()


def main() -> int:
    logging.basicConfig(
        stream=sys.stderr,
        level=logging.INFO,
        format="ui-sidecar %(levelname)s %(message)s",
    )
    from gi.repository import GLib

    # WM_CLASS comes from the program name; without this the pill shows up
    # as "python3" in window lists and insertion-target logs.
    GLib.set_prgname("voice-dictation-overlay")

    from voice_dictation.overlay import Overlay

    output_lock = threading.Lock()

    def emit(event: dict) -> None:
        with output_lock:
            sys.stdout.write(json.dumps(event, separators=(",", ":")) + "\n")
            sys.stdout.flush()

    overlay = Overlay(system_event_sink=emit)
    threading.Thread(
        target=_pump_stdin, args=(overlay,), daemon=True, name="stdin-pump"
    ).start()

    def announce_ready() -> None:
        overlay.wait_ready()
        emit({"type": "ready", "backend": "overlay"})

    threading.Thread(target=announce_ready, daemon=True, name="ready").start()
    return overlay.build_and_run()


if __name__ == "__main__":
    sys.exit(main())
