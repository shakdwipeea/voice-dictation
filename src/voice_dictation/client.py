"""vd CLI — sends a single command to the daemon socket and prints the reply.

Usage:
    vd                   # toggle (default action — what the hotkey runs)
    vd toggle
    vd status
    vd last              # show the last transcription
    vd shutdown
    vd simulate <wav>    # test hook: transcribe an existing wav
"""
from __future__ import annotations

import argparse
import json
import os
import socket
import sys
from pathlib import Path


def default_socket_path() -> Path:
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    return Path(runtime) / "voice-dictation.sock"


def send(socket_path: Path, payload: dict, timeout: float = 30.0) -> dict:
    if not socket_path.exists():
        return {"error": f"daemon not running (no socket at {socket_path})"}
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        s.connect(str(socket_path))
        s.sendall((json.dumps(payload) + "\n").encode("utf-8"))
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
        return json.loads(buf.decode("utf-8")) if buf else {"error": "empty reply"}
    except socket.timeout:
        return {"error": f"timeout after {timeout}s"}
    except (ConnectionRefusedError, FileNotFoundError) as e:
        return {"error": f"connect failed: {e}"}
    finally:
        s.close()


def main() -> int:
    p = argparse.ArgumentParser(description="voice-dictation client")
    p.add_argument("cmd", nargs="?", default="toggle",
                   choices=["toggle", "status", "last", "shutdown", "simulate"])
    p.add_argument("arg", nargs="?", default=None,
                   help="extra arg (e.g. wav path for 'simulate')")
    p.add_argument("--socket", type=Path, default=default_socket_path())
    p.add_argument("--json", action="store_true", help="print raw JSON reply")
    p.add_argument("--timeout", type=float, default=30.0)
    args = p.parse_args()

    payload: dict = {"cmd": args.cmd}
    if args.cmd == "simulate":
        if not args.arg:
            print("error: simulate needs a wav path", file=sys.stderr)
            return 2
        payload["wav"] = args.arg

    reply = send(args.socket, payload, timeout=args.timeout)
    if args.json:
        print(json.dumps(reply, indent=2))
    else:
        if "error" in reply:
            print(f"error: {reply['error']}", file=sys.stderr)
            return 1
        if args.cmd == "status":
            for k, v in reply.items():
                print(f"  {k}: {v}")
        elif args.cmd in ("toggle", "simulate"):
            action = reply.get("action", "?")
            if action == "started":
                print("recording…")
            elif action == "stopped":
                txt = reply.get("text", "")
                inf = reply.get("inference_s", 0.0)
                dur = reply.get("audio_duration_s", 0.0)
                print(f"[{dur:.1f}s audio → {inf:.2f}s] {txt}")
            else:
                print(json.dumps(reply, indent=2))
        elif args.cmd == "last":
            print(reply.get("text", ""))
        elif args.cmd == "shutdown":
            print(reply.get("action", ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
