"""GTK4 overlay for the voice-dictation daemon.

Minimal UI: a small recording dot and a thin level meter. Anchored top-center.

Anchoring backend is chosen at runtime: gtk4-layer-shell on Wayland
compositors that support it, EWMH window hints on X11 (layer-shell is a
Wayland-only protocol).

All public methods are thread-safe — they schedule UI work on the GTK main
loop via GLib.idle_add.
"""
from __future__ import annotations

import logging
import threading
from typing import Optional

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402

try:
    gi.require_version("Gtk4LayerShell", "1.0")
    from gi.repository import Gtk4LayerShell  # noqa: E402

    _LAYER_SHELL = Gtk4LayerShell.is_supported()
except (ImportError, ValueError):
    Gtk4LayerShell = None  # type: ignore[assignment]
    _LAYER_SHELL = False

log = logging.getLogger(__name__)

MARGIN_TOP = 16


class _X11Anchor:
    """EWMH fallback anchoring for X11 sessions.

    Marks the window as an always-above, sticky, unfocusable notification
    (so it never steals keyboard focus or appears in the taskbar) and pins
    it top-center after each map.
    """

    def __init__(self, window: Gtk.Window) -> None:
        from Xlib import Xatom, Xutil
        from Xlib import display as x_display

        gi.require_version("GdkX11", "4.0")
        from gi.repository import GdkX11

        window.realize()
        surface = window.get_surface()
        if not isinstance(surface, GdkX11.X11Surface):
            raise RuntimeError("X11 anchor requires an X11 surface")
        self._dpy = x_display.Display()
        self._xwin = self._dpy.create_resource_object(
            "window", surface.get_xid()
        )
        atom = self._dpy.intern_atom
        self._xwin.change_property(
            atom("_NET_WM_WINDOW_TYPE"), Xatom.ATOM, 32,
            [atom("_NET_WM_WINDOW_TYPE_NOTIFICATION")],
        )
        self._xwin.change_property(
            atom("_NET_WM_STATE"), Xatom.ATOM, 32,
            [
                atom("_NET_WM_STATE_ABOVE"),
                atom("_NET_WM_STATE_STICKY"),
                atom("_NET_WM_STATE_SKIP_TASKBAR"),
                atom("_NET_WM_STATE_SKIP_PAGER"),
            ],
        )
        self._xwin.set_wm_hints(flags=Xutil.InputHint, input=0)
        self._dpy.flush()

    def place(self) -> bool:
        geom = self._xwin.get_geometry()
        screen_w = self._dpy.screen().width_in_pixels
        self._xwin.configure(
            x=max(0, (screen_w - geom.width) // 2), y=MARGIN_TOP
        )
        self._dpy.flush()
        return False

CSS = b"""
.vd-root {
  background: rgba(10, 10, 12, 0.82);
  border-radius: 999px;
  padding: 6px 12px;
}
.vd-dot {
  color: #555;
  font-size: 12px;
  min-width: 12px;
}
.vd-dot.vd-live { color: #ef4444; }
.vd-meter trough {
  min-height: 4px;
  min-width: 140px;
  background: rgba(255, 255, 255, 0.08);
  border-radius: 99px;
  border: none;
}
.vd-meter block.filled {
  background-color: #e5e5e5;
  border-radius: 99px;
  border: none;
}
.vd-status {
  color: #d4d4d4;
  font-size: 12px;
}
"""


class Overlay:
    """One-window overlay; thread-safe interface."""

    def __init__(self) -> None:
        self.app: Optional[Gtk.Application] = None
        self.window: Optional[Gtk.ApplicationWindow] = None
        self._root: Optional[Gtk.Box] = None
        self._dot_label: Optional[Gtk.Label] = None
        self._meter: Optional[Gtk.LevelBar] = None
        self._status_label: Optional[Gtk.Label] = None
        self._x11: Optional[_X11Anchor] = None
        self._visible = False
        self._ready_evt = threading.Event()

    def wait_ready(self, timeout: Optional[float] = None) -> bool:
        """Block until the GTK window exists (for sidecar ready handshakes)."""
        return self._ready_evt.wait(timeout)

    # ---- lifecycle ----
    def build_and_run(self) -> int:
        """Run the GTK main loop. Blocks. Call from the main thread."""
        self.app = Gtk.Application(
            application_id="dev.voice-dictation.Overlay",
            flags=0,
        )
        self.app.connect("activate", self._on_activate)
        return self.app.run(None)

    def _on_activate(self, app: Gtk.Application) -> None:
        from gi.repository import Gdk
        provider = Gtk.CssProvider()
        provider.load_from_data(CSS, -1)
        Gtk.StyleContext.add_provider_for_display(
            Gdk.Display.get_default(),
            provider,
            Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION,
        )

        self.window = Gtk.ApplicationWindow(application=app)
        self.window.set_decorated(False)
        self.window.set_resizable(False)
        if _LAYER_SHELL:
            Gtk4LayerShell.init_for_window(self.window)
            Gtk4LayerShell.set_layer(self.window, Gtk4LayerShell.Layer.OVERLAY)
            Gtk4LayerShell.set_anchor(self.window, Gtk4LayerShell.Edge.TOP, True)
            Gtk4LayerShell.set_margin(self.window, Gtk4LayerShell.Edge.TOP, MARGIN_TOP)
            Gtk4LayerShell.set_keyboard_mode(self.window, Gtk4LayerShell.KeyboardMode.NONE)
            Gtk4LayerShell.set_namespace(self.window, "voice-dictation")

        root = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=10)
        root.add_css_class("vd-root")
        self._root = root

        self._dot_label = Gtk.Label(label="●")
        self._dot_label.add_css_class("vd-dot")
        root.append(self._dot_label)

        self._meter = Gtk.LevelBar()
        self._meter.add_css_class("vd-meter")
        self._meter.set_min_value(0.0)
        self._meter.set_max_value(1.0)
        self._meter.set_value(0.0)
        self._meter.set_valign(Gtk.Align.CENTER)
        root.append(self._meter)

        from gi.repository import Pango

        self._status_label = Gtk.Label(label="")
        self._status_label.add_css_class("vd-status")
        self._status_label.set_ellipsize(Pango.EllipsizeMode.START)
        self._status_label.set_max_width_chars(36)
        self._status_label.set_visible(False)
        root.append(self._status_label)

        self.window.set_child(root)
        if not _LAYER_SHELL:
            try:
                self._x11 = _X11Anchor(self.window)
            except Exception:  # noqa: BLE001
                log.exception("X11 anchoring unavailable; overlay unmanaged")
        self._hold_id = app.hold()
        self._ready_evt.set()

    # ---- thread-safe API ----
    def show(self) -> None:
        GLib.idle_add(self._do_show)

    def hide(self) -> None:
        GLib.idle_add(self._do_hide)

    def set_recording(self, elapsed_s: float, peak: float, rms: float, segment_count: int) -> None:
        GLib.idle_add(self._do_set_recording, elapsed_s, peak, rms, segment_count)

    def set_status(self, status: str) -> None:
        GLib.idle_add(self._do_set_status, status)

    def add_segment(self, text: str) -> None:
        pass

    def clear_segments(self) -> None:
        pass

    def shutdown(self) -> None:
        GLib.idle_add(self._do_shutdown)

    # ---- GTK-thread implementations ----
    def _do_show(self) -> bool:
        if self.window is not None and not self._visible:
            self.window.present()
            self._visible = True
            if self._x11 is not None:
                # Re-pin after the map request reaches the WM; layer-shell
                # handles this declaratively, X11 needs an explicit move.
                GLib.idle_add(self._x11.place)
        return False

    def _do_hide(self) -> bool:
        if self.window is not None and self._visible:
            self.window.set_visible(False)
            self._visible = False
        return False

    def _do_set_recording(self, elapsed_s: float, peak: float, rms: float, segment_count: int) -> bool:
        if self._meter is None:
            return False
        visual_level = min(1.0, max(0.0, peak * 8.0, rms * 35.0))
        self._meter.set_value(visual_level)
        if self._dot_label is not None:
            if rms >= 0.01:
                self._dot_label.add_css_class("vd-live")
            else:
                self._dot_label.remove_css_class("vd-live")
        return False

    def _do_set_status(self, status: str) -> bool:
        if self._status_label is not None:
            self._status_label.set_label(status)
            self._status_label.set_visible(bool(status))
            if self._x11 is not None and self._visible:
                # Label growth changes the pill width; keep it centered.
                GLib.idle_add(self._x11.place)
        return False

    def _do_shutdown(self) -> bool:
        if self.app is not None:
            try:
                self.app.release(self._hold_id)
            except Exception:  # noqa: BLE001
                pass
            self.app.quit()
        return False
