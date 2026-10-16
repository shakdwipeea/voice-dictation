"""Play a dictation on the real macOS overlay without dictating.

Starts a sunoto-overlay the way the daemon does, sends it the daemon's NDJSON
(show, the start hint, 20 ms sound levels, "transcribing...", pasted, hide,
then health and an error), and optionally screenshots the top of the screen
at each step. Nothing is recorded or typed; it only draws. Run it next to a
running Sunoto if you like: the two overlays do not share anything.

    python3 tools/ui/macos_overlay_demo.py                # watch it
    python3 tools/ui/macos_overlay_demo.py --style pill   # the floating pill
    python3 tools/ui/macos_overlay_demo.py --screenshots /tmp/island

Screenshots need Screen Recording for the app running this script (your
terminal). The overlay's own log, including where it put the island's
window, is printed at the end.
"""
import argparse
import json
import math
import os
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def overlay_binary(path):
    if path:
        return path
    built = ROOT / "target/release/sunoto-overlay"
    if built.is_file():
        return str(built)
    out = Path(tempfile.mkdtemp()) / "sunoto-overlay"
    print(f"building {out} ...", file=sys.stderr)
    subprocess.run(["swiftc", "-O", str(ROOT / "services/macos/sunoto-overlay.swift"), "-o", str(out)], check=True)
    return str(out)


def screen_width():
    bounds = subprocess.run(
        ["osascript", "-e", 'tell application "Finder" to get bounds of window of desktop'],
        capture_output=True, text=True,
    ).stdout
    try:
        return int(bounds.split(",")[2])
    except (IndexError, ValueError):
        return 1440


def speech(t):
    """Quiet for a moment, then phrases with breaths between them."""
    if t < 1.2:
        return 0.01
    envelope = max(0, math.sin(t * 2.3) * 0.6 + math.sin(t * 5.1 + 1) * 0.35 + 0.25)
    return min(1, envelope * (0.75 + 0.25 * math.sin(t * 37)))


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--overlay", help="sunoto-overlay binary (default: target/release, else build one)")
    parser.add_argument("--style", choices=["auto", "pill"], default="auto", help="SUNOTO_OVERLAY_STYLE")
    parser.add_argument("--screenshots", metavar="DIR", help="save the top of the screen at each step")
    args = parser.parse_args()

    env = dict(os.environ, SUNOTO_OVERLAY_STYLE=args.style)
    proc = subprocess.Popen(
        [overlay_binary(args.overlay)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True, env=env,
    )
    ready = proc.stdout.readline().strip()
    if '"ready"' not in ready:
        sys.exit(f"overlay did not start: {ready!r}")
    log = []
    threading.Thread(target=lambda: log.extend(line.rstrip() for line in proc.stderr), daemon=True).start()

    def send(**message):
        proc.stdin.write(json.dumps(message) + "\n")
        proc.stdin.flush()

    shots = []
    if args.screenshots:
        os.makedirs(args.screenshots, exist_ok=True)
        width = screen_width()
        # The notch island needs the top 90 pt; the floating pill hangs lower.
        region = f"{width // 2 - 310},0,620,{90 if args.style == 'auto' else 110}"

    def shot(name):
        if args.screenshots:
            path = os.path.join(args.screenshots, name + ".png")
            shots.append((name, subprocess.Popen(
                ["screencapture", "-x", "-R", region, path], stderr=subprocess.DEVNULL,
            )))

    steps = [
        (0.0, lambda: (send(type="show"), send(type="status", text="Tap again to paste, Esc to cancel"))),
        (0.9, lambda: shot("01-hint")),
        (2.6, lambda: shot("02-talking")),
        (4.05, lambda: (send(type="show"), send(type="status", text="transcribing..."))),
        (4.7, lambda: shot("03-working")),
        (6.0, lambda: shot("04-transcribing")),
        (6.3, lambda: (send(type="pasted"), send(type="hide"))),
        (6.95, lambda: shot("05-pasted")),
        (8.3, lambda: shot("06-closed")),
        (8.6, lambda: send(type="state", name="loading_asr", detail="loading speech model...")),
        (9.3, lambda: shot("07-loading")),
        (9.6, lambda: send(type="state", name="ready", detail="")),
        (10.6, lambda: (send(type="show"), send(type="status", text="No matching target found", error=True))),
        (11.2, lambda: shot("08-error")),
        (11.5, lambda: send(type="hide")),
    ]
    talk_until, end = 4.0, 12.2
    start = time.monotonic()
    next_level = 0.0
    while (t := time.monotonic() - start) < end:
        while steps and steps[0][0] <= t:
            steps.pop(0)[1]()
        if t < talk_until and t >= next_level:
            level = speech(t)
            send(type="recording", elapsed_s=round(t, 3), peak=level / 8, rms=level / 35, segments=0)
            next_level += 0.02
        time.sleep(0.002)
    failed = [name for name, screenshot in shots if screenshot.wait() != 0]
    if shots:
        print(f"saved {len(shots) - len(failed)} screenshots to {args.screenshots}")
    if failed:
        print(f"screenshots failed: {', '.join(failed)}. The display may be asleep, or the terminal "
              "lacks Screen Recording permission.", file=sys.stderr)
    send(type="shutdown")
    proc.wait(timeout=5)
    print("\n".join(log) or "(no overlay log)")


if __name__ == "__main__":
    main()
