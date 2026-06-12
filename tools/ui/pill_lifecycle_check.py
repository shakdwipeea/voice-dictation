"""Live X11 lifecycle check for the overlay pill.

Run on a real X session (the daemon may be running; the sidecar is
NON_UNIQUE):

    python3 tools/ui/pill_lifecycle_check.py

Spawns the UI sidecar exactly as the daemon does, drives it with the field
op sequence (including a status->hide burst with no gap — the deferred
re-center race window), and asserts the real window geometry on the X
server at each step:

  - override-redirect is set (the WM cannot manage/clamp the window)
  - parked off-screen at startup
  - on-screen, top-centered while shown
  - back at the park position after hide, and STAYS there

History: hide-by-opacity silently never repainted, and hide-by-park on a
WM-managed window got clamped back on-screen at (0,0) by Muffin. This
script is the regression check for that whole class of "the pill never
went away" bugs.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from Xlib import display as x_display

ROOT = Path(__file__).resolve().parents[2]
PARK = -16384
failures = []


def check(name, cond, detail=""):
    tag = "ok " if cond else "FAIL"
    print(f"  [{tag}] {name} {detail}")
    if not cond:
        failures.append(name)


def find_pill(dpy, pid):
    """The sidecar's pill: a root child with our WM_CLASS, our PID, real size."""
    root = dpy.screen().root
    net_wm_pid = dpy.intern_atom("_NET_WM_PID")
    for win in root.query_tree().children:
        try:
            cls = win.get_wm_class()
            if not cls or cls[0] != "voice-dictation-overlay":
                continue
            prop = win.get_full_property(net_wm_pid, 0)
            if not prop or prop.value[0] != pid:
                continue
            if win.get_geometry().width <= 10:  # GDK leader window is 1x1
                continue
            return win
        except Exception:
            continue
    return None


def geom(win):
    g = win.get_geometry()
    return g.x, g.y, g.width, g.height


def main():
    env = dict(os.environ)
    env["PYTHONPATH"] = str(ROOT / "src")
    env.setdefault("DISPLAY", ":0")

    proc = subprocess.Popen(
        ["python3", "-m", "voice_dictation.ui_sidecar"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, env=env, text=True, bufsize=1,
    )

    def send(**msg):
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()

    try:
        ready = proc.stdout.readline().strip()
        print(f"sidecar ready line: {ready}")
        check("ready handshake", '"ready"' in ready)

        time.sleep(1.0)  # let the map + startup park settle
        dpy = x_display.Display(env["DISPLAY"])
        pill = find_pill(dpy, proc.pid)
        check("pill window found", pill is not None)
        if pill is None:
            return

        attrs = pill.get_attributes()
        check("override-redirect set", attrs.override_redirect == 1)
        x, y, w, h = geom(pill)
        check("parked at startup", (x, y) == (PARK, PARK), f"at ({x},{y})")
        check("mapped (stays mapped by design)", attrs.map_state == 2)

        print("-- cycle 1: show -> levels -> partials -> status+hide burst --")
        send(type="show")
        send(type="status", text="transcribing...")
        time.sleep(0.5)
        x, y, w, h = geom(pill)
        check("on-screen while shown", x >= 0 and y >= 0, f"at ({x},{y}) {w}x{h}")
        check("anchored at top margin", y == 16, f"y={y}")

        for i in range(4):  # mid-dictation level + partial updates
            send(type="recording", elapsed_s=i * 0.3, peak=0.4, rms=0.05, segments=0)
            send(type="status", text=f"partial text tail number {i}")
            time.sleep(0.15)
        # The race: a label change immediately followed by hide. The
        # deferred re-center must not drag the parked pill back.
        send(type="status", text="final partial just before the hide lands")
        send(type="hide")
        time.sleep(0.7)
        x, y, w, h = geom(pill)
        check("parked after hide", (x, y) == (PARK, PARK), f"at ({x},{y})")
        time.sleep(1.2)  # late movers: WM clamp / deferred place / GTK relayout
        x, y, w, h = geom(pill)
        check("still parked 1.2s later", (x, y) == (PARK, PARK), f"at ({x},{y})")

        print("-- cycle 2: empty-status variant --")
        send(type="show")
        send(type="status", text="transcribing...")
        time.sleep(0.4)
        x, y, w, h = geom(pill)
        check("shown again on cycle 2", x >= 0 and y == 16, f"at ({x},{y})")
        send(type="status", text="")
        send(type="hide")
        time.sleep(0.9)
        x, y, w, h = geom(pill)
        check("parked after cycle 2 hide", (x, y) == (PARK, PARK), f"at ({x},{y})")

        send(type="shutdown")
        proc.wait(timeout=5)
        check("clean exit", proc.returncode == 0, f"rc={proc.returncode}")
    finally:
        if proc.poll() is None:
            proc.kill()
        err = proc.stderr.read()
        if err:
            print("-- sidecar stderr --")
            print(err.strip())

    print(f"\n{'ALL CHECKS PASSED' if not failures else 'FAILURES: ' + ', '.join(failures)}")
    sys.exit(1 if failures else 0)


main()
