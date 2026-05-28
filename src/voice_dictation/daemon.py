"""voice-dictation daemon (M3).

Single long-running process that:
  - Holds the faster-whisper model warm.
  - Owns a GTK4 layer-shell overlay (main thread) when not --no-overlay.
  - Runs a Unix-socket IPC server on a worker thread.
  - Toggles a streaming pipeline (silero-VAD segmented → whisper → overlay).
  - On stop, joins all segments and pastes (unless --no-paste).

IPC: newline-delimited JSON over $XDG_RUNTIME_DIR/voice-dictation.sock.

Requests:
    {"cmd": "toggle"}
    {"cmd": "status"}
    {"cmd": "last"}
    {"cmd": "shutdown"}
    {"cmd": "simulate", "wav": "/path/to/file.wav"}    # batch test hook
"""
from __future__ import annotations

import argparse
import json
import logging
import os
import signal
import socket
import socketserver
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

import numpy as np
import soundfile as sf

from voice_dictation.inject import paste_text
from voice_dictation.pipeline import StreamingPipeline
from voice_dictation.stt import Transcriber

log = logging.getLogger("vd.daemon")


def default_socket_path() -> Path:
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    return Path(runtime) / "voice-dictation.sock"


STATE_DIR = Path(
    os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state"
) / "voice-dictation"
STATE_DIR.mkdir(parents=True, exist_ok=True)


@dataclass
class DaemonConfig:
    model: str = "large-v3-turbo"
    device: str = "cuda"
    no_paste: bool = False
    no_overlay: bool = False
    socket_path: Path = field(default_factory=default_socket_path)
    input_device: object = None  # forwarded to pick_input()


@dataclass
class LastResult:
    text: str = ""
    audio_duration_s: float = 0.0
    inference_s: float = 0.0
    paste: dict = field(default_factory=dict)
    timestamp: float = 0.0


class Daemon:
    def __init__(self, cfg: DaemonConfig) -> None:
        self.cfg = cfg
        self.overlay = None  # set later if not --no-overlay
        self.transcriber: Optional[Transcriber] = None
        self.pipeline: Optional[StreamingPipeline] = None
        self.last_result = LastResult()
        self._lock = threading.Lock()
        self._stop_evt = threading.Event()
        self._toggle_t_start: float = 0.0  # for measuring toggle-to-paste latency

    def attach_overlay(self, overlay) -> None:  # type: ignore[no-untyped-def]
        self.overlay = overlay

    def init_pipeline(self) -> None:
        if self.transcriber is None:
            self.transcriber = Transcriber(
                model_name=self.cfg.model, device=self.cfg.device
            )
        if self.pipeline is None:
            self.pipeline = StreamingPipeline(
                self.transcriber,
                overlay=self.overlay,
                input_device=self.cfg.input_device,
            )

    # ---- handlers ----
    def status(self) -> dict:
        return {
            "state": "RECORDING" if (self.pipeline and self.pipeline.is_recording()) else "IDLE",
            "elapsed_s": self.pipeline.elapsed() if self.pipeline else 0.0,
            "model": self.cfg.model,
            "device": self.cfg.device,
            "no_paste": self.cfg.no_paste,
            "no_overlay": self.cfg.no_overlay,
            "last_text": self.last_result.text,
            "model_loaded": self.transcriber is not None,
            "pipeline_ready": self.pipeline is not None,
        }

    def toggle(self) -> dict:
        with self._lock:
            if self.pipeline is None:
                self.init_pipeline()
            assert self.pipeline is not None
            if not self.pipeline.is_recording():
                self._toggle_t_start = time.perf_counter()
                self.pipeline.start_recording()
                log.info("recording started")
                return {"action": "started"}

        # Stop is outside the lock so we don't hold it during whisper drain.
        joined, recording_s = self.pipeline.stop_recording()
        total_s = time.perf_counter() - self._toggle_t_start
        log.info("recording stopped (rec=%.2fs total=%.2fs), joined text=%r",
                 recording_s, total_s, joined)

        paste_info: dict = {}
        if joined and not self.cfg.no_paste:
            paste_info = paste_text(joined)
            log.info("paste: %s", paste_info)

        self.last_result = LastResult(
            text=joined,
            audio_duration_s=recording_s,
            inference_s=total_s - recording_s,
            paste=paste_info,
            timestamp=time.time(),
        )
        try:
            (STATE_DIR / "last-text.txt").write_text((joined or "") + "\n")
        except OSError:
            pass

        return {
            "action": "stopped",
            "text": joined,
            "audio_duration_s": recording_s,
            "total_s": total_s,
            "paste": paste_info,
        }

    def simulate(self, wav_path: str) -> dict:
        """Batch transcribe a wav (test hook)."""
        path = Path(wav_path)
        if not path.exists():
            return {"error": f"wav not found: {path}"}
        data, sr = sf.read(str(path), dtype="float32")
        if data.ndim > 1:
            data = data.mean(axis=1)
        if sr != 16_000:
            return {"error": f"sample rate {sr} != 16000; resample first"}
        if self.pipeline is None:
            self.init_pipeline()
        assert self.pipeline is not None
        t0 = time.perf_counter()
        text = self.pipeline.transcribe_wav(np.asarray(data, dtype="float32"))
        inf = time.perf_counter() - t0

        paste_info: dict = {}
        if text and not self.cfg.no_paste:
            paste_info = paste_text(text)

        self.last_result = LastResult(
            text=text,
            audio_duration_s=len(data) / 16_000,
            inference_s=inf,
            paste=paste_info,
            timestamp=time.time(),
        )
        return {
            "action": "stopped",
            "text": text,
            "audio_duration_s": len(data) / 16_000,
            "inference_s": inf,
            "paste": paste_info,
        }

    def shutdown(self) -> None:
        if self.pipeline is not None:
            self.pipeline.shutdown()
        if self.overlay is not None:
            self.overlay.shutdown()
        self._stop_evt.set()


# ---- IPC server ----
class _Handler(socketserver.StreamRequestHandler):
    def handle(self) -> None:  # type: ignore[override]
        daemon: Daemon = self.server.daemon  # type: ignore[attr-defined]
        try:
            raw = self.rfile.readline()
            if not raw:
                return
            req = json.loads(raw.decode("utf-8"))
            cmd = req.get("cmd")
            if cmd == "toggle":
                resp = daemon.toggle()
            elif cmd == "status":
                resp = daemon.status()
            elif cmd == "last":
                lr = daemon.last_result
                resp = {
                    "text": lr.text,
                    "audio_duration_s": lr.audio_duration_s,
                    "inference_s": lr.inference_s,
                    "timestamp": lr.timestamp,
                    "paste": lr.paste,
                }
            elif cmd == "shutdown":
                daemon.shutdown()
                resp = {"action": "shutting_down"}
            elif cmd == "simulate":
                resp = daemon.simulate(req.get("wav", ""))
            else:
                resp = {"error": f"unknown cmd: {cmd!r}"}
        except json.JSONDecodeError as e:
            resp = {"error": f"bad json: {e}"}
        except Exception as e:  # noqa: BLE001
            log.exception("handler error")
            resp = {"error": f"{type(e).__name__}: {e}"}
        self.wfile.write((json.dumps(resp) + "\n").encode("utf-8"))


class _UnixServer(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True
    allow_reuse_address = True


def _start_ipc(daemon: Daemon) -> tuple[_UnixServer, threading.Thread]:
    sock_path = daemon.cfg.socket_path
    sock_path.parent.mkdir(parents=True, exist_ok=True)
    if sock_path.exists():
        sock_path.unlink()
    server = _UnixServer(str(sock_path), _Handler)
    server.daemon = daemon  # type: ignore[attr-defined]
    os.chmod(sock_path, 0o600)
    log.info("listening on %s", sock_path)
    t = threading.Thread(target=server.serve_forever, name="ipc", daemon=True)
    t.start()
    return server, t


_GTK4_LAYER_SHELL_LIB = "/usr/lib/libgtk4-layer-shell.so"


def _ensure_layer_shell_preload(argv: list[str]) -> None:
    """gtk4-layer-shell must load BEFORE libwayland-client or the layer-shell
    Wayland protocol never registers. When PyGObject imports GTK first, it pulls
    libwayland-client in, and our later 'import Gtk4LayerShell' is too late.
    Workaround: re-exec ourselves with LD_PRELOAD set so the lib loads first.
    Guard with an env var to avoid an infinite re-exec loop.
    """
    if os.environ.get("VD_LAYER_PRELOADED") == "1":
        return
    if "--no-overlay" in argv:
        return
    if not os.path.exists(_GTK4_LAYER_SHELL_LIB):
        return
    env = dict(os.environ)
    env["VD_LAYER_PRELOADED"] = "1"
    existing = env.get("LD_PRELOAD", "").strip()
    env["LD_PRELOAD"] = f"{_GTK4_LAYER_SHELL_LIB}:{existing}".rstrip(":")
    # Re-exec with the same interpreter/args
    os.execvpe(sys.executable, [sys.executable, "-m", "voice_dictation.daemon", *argv[1:]], env)


def main() -> int:
    _ensure_layer_shell_preload(sys.argv)
    p = argparse.ArgumentParser(description="voice-dictation daemon")
    p.add_argument("--model", default="large-v3-turbo")
    p.add_argument("--device", default="cuda", choices=["cuda", "cpu"])
    p.add_argument("--no-paste", action="store_true",
                   help="skip clipboard+paste injection")
    p.add_argument("--no-overlay", action="store_true",
                   help="run headless (no GTK overlay; e.g. for tests/SSH)")
    p.add_argument("--socket", type=Path, default=default_socket_path())
    p.add_argument("--input-device", default=None,
                   help="override input device (e.g. 'hw:2,0', 'default', or numeric index)")
    p.add_argument("--lazy-load", action="store_true",
                   help="defer model load until first request (default: eager)")
    p.add_argument("--log-level", default="INFO")
    args = p.parse_args()

    logging.basicConfig(
        level=args.log_level,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    )

    cfg = DaemonConfig(
        model=args.model,
        device=args.device,
        no_paste=args.no_paste,
        no_overlay=args.no_overlay,
        socket_path=args.socket,
        input_device=args.input_device,
    )
    daemon = Daemon(cfg)

    # Overlay setup (must own main thread if enabled)
    overlay = None
    if not cfg.no_overlay:
        from voice_dictation.overlay import Overlay  # imported lazily to avoid GI cost in headless
        overlay = Overlay()
        daemon.attach_overlay(overlay)

    if not args.lazy_load:
        # In overlay mode, do model load on a worker so GTK can start fast.
        threading.Thread(target=daemon.init_pipeline, name="model-loader", daemon=True).start()
    # (lazy: init_pipeline runs on first toggle/simulate)

    server, _ipc_thread = _start_ipc(daemon)

    def _on_signal(_sig, _frame):  # noqa: ANN001
        log.info("signal received, shutting down")
        daemon.shutdown()
    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    try:
        if overlay is not None:
            # Blocks until overlay.shutdown() is called via GLib.idle_add
            overlay.build_and_run()
        else:
            daemon._stop_evt.wait()
    finally:
        log.info("shutting down ipc server")
        server.shutdown()
        server.server_close()
        try:
            cfg.socket_path.unlink()
        except FileNotFoundError:
            pass
        log.info("daemon stopped")
    return 0


if __name__ == "__main__":
    sys.exit(main())
