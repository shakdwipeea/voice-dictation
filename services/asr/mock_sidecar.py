#!/usr/bin/env python3
"""Protocol-compatible mock ASR sidecar for the Phase 1 daemon slice.

Mimics the asynchronous behavior of the real streaming sidecar: partial
events can arrive between requests, finals can be delayed, and stale audio
chunks are ignored silently (the daemon may keep streaming for a session the
sidecar already dropped; replying to each stale chunk would flood stdout).
"""

from __future__ import annotations

import argparse
import json
import sys
import time


def emit(event: dict[str, object]) -> None:
    print(json.dumps(event, separators=(",", ":")), flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--final-text", default="Sunoto Phase 1 insertion works.")
    parser.add_argument(
        "--partial-every-chunks",
        type=int,
        default=0,
        help="emit a growing partial after every N audio chunks (0 disables)",
    )
    parser.add_argument(
        "--final-delay-ms",
        type=int,
        default=0,
        help="simulated processing delay before the final event",
    )
    args = parser.parse_args()

    words = args.final_text.split()
    active_session: int | None = None
    chunks = 0
    partials = 0

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError:
            emit({"type": "error", "session_id": None, "message": "invalid request: bad JSON"})
            continue
        if not isinstance(request, dict):
            emit({"type": "error", "session_id": None, "message": "invalid request: not an object"})
            continue
        request_type = request.get("type")
        session_id = request.get("session_id")

        if request_type == "health":
            emit({"type": "ready", "backend": "mock"})
        elif request_type == "start_session" and isinstance(session_id, int):
            active_session = session_id
            chunks = 0
            partials = 0
            emit({"type": "session_started", "session_id": session_id})
        elif request_type == "audio_chunk":
            if session_id != active_session:
                continue
            chunks += 1
            if args.partial_every_chunks > 0 and chunks % args.partial_every_chunks == 0:
                partials = min(partials + 1, len(words))
                if partials > 0:
                    emit(
                        {
                            "type": "partial",
                            "session_id": session_id,
                            "text": " ".join(words[:partials]),
                        }
                    )
        elif request_type == "finish_session" and session_id == active_session:
            if args.final_delay_ms > 0:
                time.sleep(args.final_delay_ms / 1000)
            emit({"type": "final", "session_id": session_id, "text": args.final_text})
            active_session = None
        elif request_type == "cancel_session" and session_id == active_session:
            active_session = None
            emit({"type": "error", "session_id": session_id, "message": "session cancelled"})
        else:
            emit(
                {
                    "type": "error",
                    "session_id": session_id if isinstance(session_id, int) else None,
                    "message": f"invalid request: {request_type}",
                }
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
