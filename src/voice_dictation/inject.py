"""Inject text into the focused window via clipboard + paste shortcut.

Strategy on Hyprland/Wayland:
  1. Save current clipboard (wl-paste).
  2. wl-copy the new text.
  3. Detect active window class via hyprctl activewindow -j.
     - Terminal classes  → send Ctrl+Shift+V.
     - Everything else  → send Ctrl+V.
  4. Restore prior clipboard after a short delay.
"""
from __future__ import annotations

import json
import logging
import re
import shutil
import subprocess
import threading
import time
from typing import Optional

log = logging.getLogger(__name__)

TERMINAL_CLASS_REGEX = re.compile(
    r"^(ghostty|com\.mitchellh\.ghostty|alacritty|kitty|foot|wezterm|xterm|st|gnome-terminal|konsole)$",
    re.IGNORECASE,
)
RESTORE_DELAY_S = 1.0  # how long to wait before restoring clipboard after paste succeeds


def _has(cmd: str) -> bool:
    return shutil.which(cmd) is not None


def get_active_window_class() -> Optional[str]:
    if not _has("hyprctl"):
        return None
    try:
        r = subprocess.run(
            ["hyprctl", "activewindow", "-j"],
            capture_output=True, text=True, timeout=1.0,
        )
        if r.returncode != 0:
            return None
        data = json.loads(r.stdout)
        return data.get("class") or None
    except (subprocess.TimeoutExpired, json.JSONDecodeError, OSError):
        return None


def is_terminal(window_class: Optional[str]) -> bool:
    if not window_class:
        return False
    return bool(TERMINAL_CLASS_REGEX.match(window_class))


def read_clipboard() -> Optional[bytes]:
    if not _has("wl-paste"):
        return None
    try:
        r = subprocess.run(
            ["wl-paste", "--no-newline"],
            capture_output=True, timeout=1.0,
        )
        return r.stdout if r.returncode == 0 else None
    except (subprocess.TimeoutExpired, OSError):
        return None


def write_clipboard(text: str) -> bool:
    if not _has("wl-copy"):
        log.warning("wl-copy not found")
        return False
    try:
        subprocess.run(["wl-copy"], input=text, text=True, timeout=2.0, check=True)
        return True
    except (subprocess.TimeoutExpired, subprocess.CalledProcessError, OSError) as e:
        log.warning("wl-copy failed: %s", e)
        return False


def write_clipboard_bytes(data: bytes) -> bool:
    if not _has("wl-copy") or data is None:
        return False
    try:
        subprocess.run(["wl-copy"], input=data, timeout=2.0, check=True)
        return True
    except (subprocess.TimeoutExpired, subprocess.CalledProcessError, OSError):
        return False


def send_paste_shortcut(use_shift: bool) -> bool:
    """Dispatch the paste keystroke to the focused window via Hyprland."""
    if not _has("hyprctl"):
        log.warning("hyprctl not found — cannot send paste shortcut")
        return False
    mods = "CTRL_SHIFT" if use_shift else "CTRL"
    # Hyprland syntax: sendshortcut MODS, KEY, WINDOW (empty WINDOW = focused)
    # The comma-separated fields must not contain spaces: `CTRL,V,` works,
    # while `CTRL, V,` can be parsed as a different/invalid key on Hyprland.
    arg = f"{mods},V,"
    try:
        r = subprocess.run(
            ["hyprctl", "dispatch", "sendshortcut", arg],
            capture_output=True, text=True, timeout=1.0,
        )
        ok = r.returncode == 0 and "ok" in (r.stdout.lower() if r.stdout else "")
        if not ok:
            log.warning("sendshortcut returned: rc=%d stdout=%r stderr=%r",
                        r.returncode, r.stdout, r.stderr)
        return ok
    except (subprocess.TimeoutExpired, OSError) as e:
        log.warning("sendshortcut failed: %s", e)
        return False


def type_text_direct(text: str) -> bool:
    """Type text via wtype if available (Wayland virtual-keyboard fallback)."""
    if not _has("wtype"):
        return False
    try:
        subprocess.run(["wtype", "-"], input=text, text=True, timeout=5.0, check=True)
        return True
    except (subprocess.TimeoutExpired, subprocess.CalledProcessError, OSError) as e:
        log.warning("wtype failed: %s", e)
        return False


def paste_text(text: str, *, preserve_clipboard: bool = True) -> dict:
    """Full inject flow. Returns a dict describing what happened (for logs/tests)."""
    result = {
        "text_len": len(text),
        "active_class": None,
        "used_shift_paste": False,
        "clipboard_saved": False,
        "clipboard_written": False,
        "shortcut_sent": False,
        "typed_direct": False,
        "clipboard_restored_scheduled": False,
    }
    if not text:
        return result

    active = get_active_window_class()
    result["active_class"] = active
    use_shift = is_terminal(active)
    result["used_shift_paste"] = use_shift

    saved = read_clipboard() if preserve_clipboard else None
    result["clipboard_saved"] = saved is not None

    if not write_clipboard(text):
        return result
    result["clipboard_written"] = True

    # Tiny gap so the compositor has the new clipboard owner before paste fires.
    time.sleep(0.05)
    result["shortcut_sent"] = send_paste_shortcut(use_shift)
    if not result["shortcut_sent"]:
        result["typed_direct"] = type_text_direct(text)

    if preserve_clipboard and saved is not None and result["shortcut_sent"]:
        def _restore():
            time.sleep(RESTORE_DELAY_S)
            write_clipboard_bytes(saved)
        threading.Thread(target=_restore, daemon=True).start()
        result["clipboard_restored_scheduled"] = True

    return result
