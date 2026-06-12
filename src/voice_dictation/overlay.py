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
import os
import threading
from typing import Optional

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402

try:
    gi.require_version("Gtk4LayerShell", "1.0")
    from gi.repository import Gtk4LayerShell  # noqa: E402

    _LAYER_SHELL_AVAILABLE = True
except (ImportError, ValueError):
    Gtk4LayerShell = None  # type: ignore[assignment]
    _LAYER_SHELL_AVAILABLE = False

log = logging.getLogger(__name__)

MARGIN_TOP = 16
OVERLAY_BACKENDS = {"auto", "x11", "wayland"}


def _layer_shell_supported() -> bool:
    if not _LAYER_SHELL_AVAILABLE:
        return False
    try:
        return bool(Gtk4LayerShell.is_supported())
    except Exception:  # noqa: BLE001 - display/backend dependent
        return False


def _display_backend() -> str:
    from gi.repository import Gdk

    display = Gdk.Display.get_default()
    if display is None:
        return "unknown"
    try:
        gi.require_version("GdkWayland", "4.0")
        from gi.repository import GdkWayland

        if isinstance(display, GdkWayland.WaylandDisplay):
            return "wayland"
    except (ImportError, ValueError, AttributeError):
        pass
    try:
        gi.require_version("GdkX11", "4.0")
        from gi.repository import GdkX11

        if isinstance(display, GdkX11.X11Display):
            return "x11"
    except (ImportError, ValueError, AttributeError):
        pass
    return "unknown"


class _X11Anchor:
    """X11 anchoring: an override-redirect toplevel, positioned by hand.

    Override-redirect takes the window manager out of the loop entirely:
    no map-time focus decisions, and — critically — no constraint pass
    that "rescues" off-screen windows. While the pill was WM-managed,
    the park() move below reached Muffin as a ConfigureRequest and its
    keep-windows-on-screen policy clamped the pill to (0,0), so it sat
    in the corner forever after every dictation. The EWMH properties are
    kept for the compositor (effect/shadow policy), not for the WM.
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
        # Must be latched before GTK maps the window, and GTK maps over
        # its own X connection — hence sync(), not flush().
        self._xwin.change_attributes(override_redirect=1)
        self._dpy.sync()
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
        self._take_focus = atom("WM_TAKE_FOCUS")
        self._strip_take_focus()
        self._make_click_through()
        # Park before the first map: override-redirect maps happen at the
        # last configured position, so the pill never flashes at (0,0).
        self.park()
        self._dpy.flush()

    def _make_click_through(self) -> None:
        """Empty X input shape: clicks fall through to whatever is below.

        The pill stays mapped permanently (visibility is position-based,
        see place/park) so it must never swallow pointer input meant for
        the window underneath it.
        """
        try:
            from Xlib.ext import shape

            self._xwin.shape_rectangles(
                shape.SO.Set, shape.SK.Input, 0, 0, 0, []
            )
        except Exception:  # noqa: BLE001 - cosmetic; never fatal
            log.warning("XShape unavailable; overlay will not be click-through")

    def _strip_take_focus(self) -> None:
        """Remove WM_TAKE_FOCUS so the window never enters the "globally
        active" focus model — with it present, the WM offers focus on map
        and dictated keystrokes land in this pill instead of the user's
        window. The input hint alone does not prevent that."""
        protocols = self._xwin.get_wm_protocols()
        if protocols and self._take_focus in protocols:
            self._xwin.set_wm_protocols(
                [p for p in protocols if p != self._take_focus]
            )

    def _primary_monitor(self) -> tuple:
        """(x, y, width) of the primary monitor; whole X screen on failure.

        Centering on the combined X screen puts the pill on the seam
        between monitors in multi-head setups.
        """
        try:
            from Xlib.ext import randr

            monitors = randr.get_monitors(
                self._dpy.screen().root, True
            ).monitors
            primary = next((m for m in monitors if m.primary), monitors[0])
            return primary.x, primary.y, primary.width_in_pixels
        except Exception:  # noqa: BLE001 - no RandR 1.5, single screen, ...
            return 0, 0, self._dpy.screen().width_in_pixels

    def place(self) -> bool:
        from Xlib import X

        # GTK may rewrite WM_PROTOCOLS up to map time; re-strip on each show.
        self._strip_take_focus()
        geom = self._xwin.get_geometry()
        mon_x, mon_y, mon_w = self._primary_monitor()
        # stack_mode=Above on every placement: nothing keeps an
        # override-redirect window on top (_NET_WM_STATE_ABOVE is a WM
        # contract), and windows mapped later would bury the pill.
        # Status updates re-place, so mid-dictation occlusion self-heals.
        self._xwin.configure(
            x=mon_x + max(0, (mon_w - geom.width) // 2),
            y=mon_y + MARGIN_TOP,
            stack_mode=X.Above,
        )
        self._dpy.flush()
        return False

    def park(self) -> bool:
        """Hide by moving far off-screen. The window stays mapped (no
        map/unmap churn) and, on an override-redirect window, the server
        applies this move verbatim. While the WM still managed the pill
        it intercepted this exact move and clamped it back on-screen at
        (0,0) — a pill that never went away."""
        self._xwin.configure(x=-16384, y=-16384)
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

    def __init__(self, backend: Optional[str] = None) -> None:
        requested = (backend or os.environ.get("SUNOTO_OVERLAY_BACKEND") or "auto").lower()
        if requested not in OVERLAY_BACKENDS:
            log.warning("unknown overlay backend %r; using auto", requested)
            requested = "auto"
        self._backend = requested
        self.app: Optional[Gtk.Application] = None
        self.window: Optional[Gtk.ApplicationWindow] = None
        self._root: Optional[Gtk.Box] = None
        self._dot_label: Optional[Gtk.Label] = None
        self._meter: Optional[Gtk.LevelBar] = None
        self._status_label: Optional[Gtk.Label] = None
        self._x11: Optional[_X11Anchor] = None
        self._using_layer_shell = False
        self._wayland_overlay_unavailable = False
        self._visible = False
        self._ready_evt = threading.Event()

    def wait_ready(self, timeout: Optional[float] = None) -> bool:
        """Block until the GTK window exists (for sidecar ready handshakes)."""
        return self._ready_evt.wait(timeout)

    # ---- lifecycle ----
    def build_and_run(self) -> int:
        """Run the GTK main loop. Blocks. Call from the main thread."""
        from gi.repository import Gio

        # NON_UNIQUE: GTK application IDs are D-Bus singletons by default,
        # so a second sidecar (respawn racing a dying predecessor, manual
        # ui-demo while the daemon runs) would silently forward its
        # activation to the existing process and exit 0 without ever
        # emitting ready.
        self.app = Gtk.Application(
            application_id="dev.voice-dictation.Overlay",
            flags=Gio.ApplicationFlags.NON_UNIQUE,
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
        self.window.set_focusable(False)
        self.window.set_focus_on_click(False)

        display_backend = _display_backend()
        layer_shell = _layer_shell_supported()
        use_layer_shell = (
            display_backend == "wayland"
            and layer_shell
            and self._backend in {"auto", "wayland"}
        )
        if use_layer_shell:
            Gtk4LayerShell.init_for_window(self.window)
            Gtk4LayerShell.set_layer(self.window, Gtk4LayerShell.Layer.OVERLAY)
            Gtk4LayerShell.set_anchor(self.window, Gtk4LayerShell.Edge.TOP, True)
            Gtk4LayerShell.set_margin(self.window, Gtk4LayerShell.Edge.TOP, MARGIN_TOP)
            Gtk4LayerShell.set_keyboard_mode(self.window, Gtk4LayerShell.KeyboardMode.NONE)
            Gtk4LayerShell.set_namespace(self.window, "voice-dictation")
            self._using_layer_shell = True
        elif self._backend == "wayland":
            log.warning(
                "Wayland overlay requested but gtk4-layer-shell is not supported; "
                "overlay disabled to avoid stealing keyboard focus"
            )
            self._wayland_overlay_unavailable = True
        elif display_backend == "wayland":
            log.warning(
                "Wayland display has no working gtk4-layer-shell support; "
                "overlay disabled to avoid stealing keyboard focus"
            )
            self._wayland_overlay_unavailable = True

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
        if not self._using_layer_shell and (
            self._backend == "x11" or (self._backend == "auto" and display_backend == "x11")
        ):
            try:
                self._x11 = _X11Anchor(self.window)
            except Exception:  # noqa: BLE001
                log.exception("X11 anchoring unavailable; overlay unmanaged")
        if self._x11 is not None:
            # Map once, parked off-screen, and never unmap. The window is
            # override-redirect, so no WM is involved in the map — and
            # staying mapped also keeps GTK's surface lifecycle out of
            # play. Visibility is position-based from here on (place/
            # park); the empty input shape keeps it click-through. The
            # idle park is belt-and-braces should GDK reposition at map.
            self.window.set_visible(True)
            GLib.idle_add(self._x11.park)
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
        log.info("show: window=%s visible=%s", self.window is not None, self._visible)
        if self._wayland_overlay_unavailable:
            return False
        if self.window is not None and not self._visible:
            if self._x11 is not None:
                # Already mapped, parked off-screen; move it into view. No
                # map request means no WM focus decision, so focus cannot
                # leave the window the user is dictating into.
                self._x11.place()
            else:
                # set_visible, NOT present(): present() requests activation,
                # which would steal keyboard focus.
                self.window.set_visible(True)
            self._visible = True
        return False

    def _do_hide(self) -> bool:
        log.info("hide: window=%s visible=%s", self.window is not None, self._visible)
        if self._wayland_overlay_unavailable:
            return False
        if self.window is not None and self._visible:
            if self._x11 is not None:
                self._x11.park()
            else:
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
                # Label growth changes the pill width; re-center one idle
                # later, once GTK has applied the new size. The callback
                # re-checks _visible: a hide can be dispatched in between,
                # and an unconditional place() would drag the freshly
                # parked pill back on-screen.
                GLib.idle_add(self._recenter_if_visible)
        return False

    def _recenter_if_visible(self) -> bool:
        if self._x11 is not None and self._visible:
            self._x11.place()
        return False

    def _do_shutdown(self) -> bool:
        if self.app is not None:
            try:
                self.app.release(self._hold_id)
            except Exception:  # noqa: BLE001
                pass
            self.app.quit()
        return False
