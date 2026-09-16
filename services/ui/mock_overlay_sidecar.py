#!/usr/bin/env python3
"""Headless NDJSON overlay used only by the System Mode E2E harness.

It records palette requests and cancels them. It never creates a window,
selects a target, or authorizes a native action.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path


def emit(payload: dict[str, object]) -> None:
    print(json.dumps(payload, separators=(",", ":")), flush=True)


def record(payload: dict[str, object]) -> None:
    destination = os.environ.get("SUNOTO_MOCK_OVERLAY_LOG")
    if not destination:
        return
    path = Path(destination)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(payload, separators=(",", ":")) + "\n")


def main() -> int:
    emit({"type": "ready", "backend": "mock-headless"})
    for line in sys.stdin:
        try:
            request = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(request, dict):
            continue
        record(request)
        request_type = request.get("type")
        if request_type == "system_palette" and isinstance(request.get("session_id"), int):
            emit({"type": "system_cancelled", "session_id": request["session_id"]})
        elif request_type == "shutdown":
            return 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
