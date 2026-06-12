"""stdin-driven UI sidecar entry point.

The Rust daemon spawns this process (managed by sunoto-ipc like the ASR
sidecar) and streams newline-delimited JSON ops on stdin:

    {"op": "show"} / {"op": "hide"}
    {"op": "recording", "elapsed": 1.2, "peak": 0.4, "rms": 0.05, "segments": 2}
    {"op": "status", "text": "transcribing"}
    {"op": "segment", "text": "..."} / {"op": "clear"}
    {"op": "shutdown"}

The sidecar emits {"event": "ready"} on stdout once the GTK window exists.
stdin EOF is equivalent to shutdown, so an exiting daemon always takes the
overlay down with it.

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
    op = msg.get("op")
    try:
        if op == "show":
            overlay.show()
        elif op == "hide":
            overlay.hide()
        elif op == "recording":
            overlay.set_recording(
                float(msg.get("elapsed", 0.0)),
                float(msg.get("peak", 0.0)),
                float(msg.get("rms", 0.0)),
                int(msg.get("segments", 0)),
            )
        elif op == "status":
            overlay.set_status(str(msg.get("text", "")))
        elif op == "segment":
            overlay.add_segment(str(msg.get("text", "")))
        elif op == "clear":
            overlay.clear_segments()
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
    from voice_dictation.overlay import Overlay

    overlay = Overlay()
    threading.Thread(
        target=_pump_stdin, args=(overlay,), daemon=True, name="stdin-pump"
    ).start()

    def announce_ready() -> None:
        overlay.wait_ready()
        sys.stdout.write(json.dumps({"event": "ready"}) + "\n")
        sys.stdout.flush()

    threading.Thread(target=announce_ready, daemon=True, name="ready").start()
    return overlay.build_and_run()


if __name__ == "__main__":
    sys.exit(main())
