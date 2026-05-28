"""Preload CUDA libs bundled by nvidia-*-cu12 Python packages.

CTranslate2 dlopens libcublas.so.12, libcudnn.so.9, etc. by short name. The
system loader won't find them in the venv unless LD_LIBRARY_PATH is set. We
preload them from their actual paths with RTLD_GLOBAL so subsequent dlopens
by short name resolve to the already-loaded library.

Call `preload()` BEFORE importing ctranslate2 / faster_whisper.
Safe no-op when device='cpu' (libs are still loaded but unused).
"""
from __future__ import annotations

import ctypes
import os
import site
import sys
from pathlib import Path

_PRELOADED = False


def _candidate_lib_dirs() -> list[Path]:
    dirs: list[Path] = []
    # uv venvs: sys.prefix/lib/pythonX.Y/site-packages
    seen: set[str] = set()
    candidates = list(site.getsitepackages())
    user_site = site.getusersitepackages()
    if user_site:
        candidates.append(user_site)
    # Also try sys.prefix-based path (uv sometimes hides getsitepackages)
    candidates.append(str(Path(sys.prefix) / "lib" / f"python{sys.version_info.major}.{sys.version_info.minor}" / "site-packages"))
    for sp in candidates:
        nvidia_root = Path(sp) / "nvidia"
        if not nvidia_root.is_dir():
            continue
        for sub in sorted(nvidia_root.iterdir()):
            lib_dir = sub / "lib"
            if lib_dir.is_dir() and str(lib_dir) not in seen:
                seen.add(str(lib_dir))
                dirs.append(lib_dir)
    return dirs


def preload(verbose: bool = False) -> list[str]:
    """Load every .so under nvidia/*/lib/ with RTLD_GLOBAL. Returns loaded names."""
    global _PRELOADED
    if _PRELOADED:
        return []
    loaded: list[str] = []
    for lib_dir in _candidate_lib_dirs():
        for so in sorted(lib_dir.iterdir()):
            name = so.name
            # Match libfoo.so or libfoo.so.N(.M.K) — skip .alt and non-shared files
            if ".alt.so" in name or ".alt" in name.split(".so", 1)[-1]:
                continue
            if not (name.endswith(".so") or ".so." in name):
                continue
            try:
                ctypes.CDLL(str(so), mode=ctypes.RTLD_GLOBAL)
                loaded.append(name)
                if verbose:
                    print(f"  preloaded {so}", file=sys.stderr)
            except OSError as e:
                if verbose:
                    print(f"  skip {so}: {e}", file=sys.stderr)
    _PRELOADED = True
    return loaded


if __name__ == "__main__":
    loaded = preload(verbose=True)
    print(f"\npreloaded {len(loaded)} libs")
