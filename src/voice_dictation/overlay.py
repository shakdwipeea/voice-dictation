"""GTK4 layer-shell overlay for the voice-dictation daemon.

Shows recording state, elapsed time, mic level meter, and finalized segments
as they arrive. Lives on the OVERLAY layer anchored top-center.

All public methods are thread-safe — they schedule UI work on the GTK main
loop via GLib.idle_add.
"""
from __future__ import annotations

import logging
import threading
from typing import Optional

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gtk4LayerShell", "1.0")
from gi.repository import GLib, Gtk, Gtk4LayerShell  # noqa: E402

log = logging.getLogger(__name__)

CSS = b"""
.vd-root {
  background: linear-gradient(135deg, rgba(8, 10, 14, 0.94), rgba(20, 26, 35, 0.92));
  border: 1px solid rgba(148, 163, 184, 0.24);
  border-radius: 24px;
  padding: 14px 18px;
  color: #eaeaee;
  box-shadow: 0 24px 70px rgba(0, 0, 0, 0.48), inset 0 1px 0 rgba(255, 255, 255, 0.08);
}
.vd-root.vd-live {
  border-color: rgba(74, 222, 128, 0.48);
  background: linear-gradient(135deg, rgba(5, 18, 14, 0.95), rgba(17, 35, 27, 0.92));
}
.vd-root.vd-hot {
  border-color: rgba(248, 113, 113, 0.58);
  background: linear-gradient(135deg, rgba(24, 10, 10, 0.96), rgba(42, 25, 12, 0.92));
}
.vd-orb {
  color: #334155;
  font-size: 42px;
  min-width: 44px;
  text-shadow: 0 0 18px rgba(51, 65, 85, 0.7);
}
.vd-orb.vd-live {
  color: #34d399;
  text-shadow: 0 0 22px rgba(52, 211, 153, 0.95), 0 0 54px rgba(52, 211, 153, 0.45);
}
.vd-orb.vd-hot {
  color: #fb7185;
  text-shadow: 0 0 24px rgba(251, 113, 133, 0.98), 0 0 60px rgba(251, 191, 36, 0.35);
}
.vd-meter trough { min-height: 16px; min-width: 430px; background: rgba(15, 23, 42, 0.94); border-radius: 99px; border: 1px solid rgba(255,255,255,0.08); }
.vd-meter highlight { background: linear-gradient(to right, #06b6d4, #22c55e, #facc15, #fb7185); border-radius: 99px; }
.vd-bars {
  font-family: "JetBrains Mono", "Cascadia Mono", "monospace";
  font-size: 28px;
  font-weight: 700;
  letter-spacing: 3px;
  color: #475569;
  text-shadow: 0 0 18px rgba(71, 85, 105, 0.4);
}
.vd-bars.vd-live {
  color: #4ade80;
  text-shadow: 0 0 18px rgba(74, 222, 128, 0.65);
}
.vd-bars.vd-hot {
  color: #facc15;
  text-shadow: 0 0 20px rgba(250, 204, 21, 0.72);
}
"""


class Overlay:
    """One-window overlay; thread-safe interface."""

    def __init__(self) -> None:
        self.app: Optional[Gtk.Application] = None
        self.window: Optional[Gtk.ApplicationWindow] = None
        self._root: Optional[Gtk.Box] = None
        self._orb_label: Optional[Gtk.Label] = None
        self._bars_label: Optional[Gtk.Label] = None
        self._meter: Optional[Gtk.LevelBar] = None
        self._visible = False
        self._ready_evt = threading.Event()

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

        # Build window. Layer-shell init MUST happen immediately after window
        # construction and BEFORE any size/decoration calls or set_visible().
        self.window = Gtk.ApplicationWindow(application=app)
        Gtk4LayerShell.init_for_window(self.window)
        Gtk4LayerShell.set_layer(self.window, Gtk4LayerShell.Layer.OVERLAY)
        Gtk4LayerShell.set_anchor(self.window, Gtk4LayerShell.Edge.TOP, True)
        Gtk4LayerShell.set_margin(self.window, Gtk4LayerShell.Edge.TOP, 32)
        Gtk4LayerShell.set_keyboard_mode(self.window, Gtk4LayerShell.KeyboardMode.NONE)
        Gtk4LayerShell.set_namespace(self.window, "voice-dictation")
        self.window.set_decorated(False)
        self.window.set_resizable(False)

        root = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=16)
        root.add_css_class("vd-root")
        self._root = root

        self._orb_label = Gtk.Label(label="●")
        self._orb_label.add_css_class("vd-orb")
        root.append(self._orb_label)

        meters = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        root.append(meters)

        self._meter = Gtk.LevelBar()
        self._meter.add_css_class("vd-meter")
        self._meter.set_min_value(0.0)
        self._meter.set_max_value(1.0)
        self._meter.set_value(0.0)
        meters.append(self._meter)

        self._bars_label = Gtk.Label(label="▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁")
        self._bars_label.add_css_class("vd-bars")
        self._bars_label.set_xalign(0.0)
        meters.append(self._bars_label)

        self.window.set_child(root)
        # Hold the application alive even though the window starts hidden.
        # (Calling window.set_visible(False) before any show would tear down
        # the layer-shell surface; better to just never present it yet.)
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
        pass

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
        self._set_visual_state(visual_level, rms)
        if self._bars_label is not None:
            self._bars_label.set_label(self._bars_for_level(visual_level, elapsed_s))
        return False

    def _set_visual_state(self, level: float, rms: float) -> None:
        state = "hot" if level > 0.78 else "live" if rms >= 0.01 else "quiet"
        for widget in (self._root, self._orb_label, self._bars_label):
            if widget is None:
                continue
            widget.remove_css_class("vd-live")
            widget.remove_css_class("vd-hot")
            if state == "live":
                widget.add_css_class("vd-live")
            elif state == "hot":
                widget.add_css_class("vd-hot")

    def _bars_for_level(self, level: float, elapsed_s: float) -> str:
        blocks = "▁▂▃▄▅▆▇█"
        count = 18
        active = max(0, min(count, round(level * count)))
        if active == 0:
            pulse = int(elapsed_s * 10) % count
            return "".join("▂" if i == pulse else "▁" for i in range(count))
        wave = int(elapsed_s * 18)
        out = []
        for i in range(count):
            if i >= active:
                out.append("▁")
                continue
            crest = (i + wave) % len(blocks)
            energy = min(len(blocks) - 1, max(1, round((i + 1) / max(active, 1) * (len(blocks) - 1))))
            out.append(blocks[max(energy, crest if level > 0.2 else 1)])
        return "".join(out)

    def _do_shutdown(self) -> bool:
        if self.app is not None:
            try:
                self.app.release(self._hold_id)
            except Exception:  # noqa: BLE001
                pass
            self.app.quit()
        return False
