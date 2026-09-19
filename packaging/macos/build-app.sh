#!/usr/bin/env bash
# Build a self-contained Sunoto.app for Apple silicon and zip it for release.
#
# Output (in $OUT, default dist/):
#   Sunoto.app                  daemon + overlay + embedded runtime
#   Sunoto-macos-arm64.zip      ditto archive of the bundle
#   Sunoto-macos-arm64.zip.sha256
#
# The bundle carries everything the daemon resolves relative to its root
# (see apps/daemon/src/setup.rs, EMBEDDED_ROOT):
#   Contents/Resources/root/services, src        sidecar scripts
#   Contents/Resources/root/python               relocatable CPython 3.12
#                                                with parakeet-mlx, mlx and
#                                                llama-cpp-python (Metal)
#   Contents/Resources/root/.venv-nemotron-mac/bin/python   -> python
#   Contents/Resources/root/.venv-llm-polish-mac/bin/python -> python
# Models are not bundled; `sunoto-daemon setup` downloads them with progress.
#
# Requires: cargo, swiftc (Xcode command line tools), cmake, curl, codesign.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${OUT:-$ROOT/dist}"
APP="$OUT/Sunoto.app"
ZIP="$OUT/Sunoto-macos-arm64.zip"

# Pinned runtime. Update the three lines together.
PBS_TAG="20260901"
PBS_VERSION="3.12.14"
PBS_ASSET="cpython-${PBS_VERSION}+${PBS_TAG}-aarch64-apple-darwin-install_only.tar.gz"
PBS_BASE="https://github.com/astral-sh/python-build-standalone/releases/download/${PBS_TAG}"
# GitHub serves the '+' in the asset name only when it is percent-encoded.
PBS_URL="${PBS_BASE}/${PBS_ASSET//+/%2B}"
PBS_SUMS_URL="${PBS_BASE}/SHA256SUMS"

# Pinned packages: the versions the project was benchmarked with.
ASR_PACKAGES=("parakeet-mlx==0.5.2" "mlx==0.31.2" "huggingface_hub>=0.30.2")
LLM_PACKAGE="llama-cpp-python==0.3.31"

bold() { printf '\033[1m== %s ==\033[0m\n' "$*"; }
ok()   { printf '\033[32m  ✓\033[0m %s\n' "$*"; }
fail() { printf '\033[31m  ✗\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(uname -m)" = "arm64" ] || fail "build on Apple silicon (uname -m = arm64)"
for tool in cargo swiftc cmake curl codesign ditto shasum; do
    command -v "$tool" >/dev/null || fail "$tool not found"
done

bold "build daemon and overlay"
(cd "$ROOT" && cargo build --release -p sunoto-daemon)
swiftc -O "$ROOT/services/macos/sunoto-overlay.swift" -o "$ROOT/target/release/sunoto-overlay"
ok "target/release/sunoto-daemon, sunoto-overlay"

bold "embedded Python runtime"
CACHE="${SUNOTO_BUILD_CACHE:-$ROOT/build/macos-app}"
mkdir -p "$CACHE"
if [ ! -f "$CACHE/$PBS_ASSET" ]; then
    curl -fL --progress-bar -o "$CACHE/$PBS_ASSET.part" "$PBS_URL"
    curl -fsSL -o "$CACHE/SHA256SUMS" "$PBS_SUMS_URL"
    expected="$(grep " $PBS_ASSET\$" "$CACHE/SHA256SUMS" | head -1 | cut -d' ' -f1)"
    [ -n "$expected" ] || fail "$PBS_ASSET not listed in SHA256SUMS"
    actual="$(shasum -a 256 "$CACHE/$PBS_ASSET.part" | cut -d' ' -f1)"
    [ "$expected" = "$actual" ] || fail "python runtime checksum mismatch"
    mv "$CACHE/$PBS_ASSET.part" "$CACHE/$PBS_ASSET"
fi
ok "python-build-standalone $PBS_VERSION ($PBS_TAG) verified"

rm -rf "$APP"
RES="$APP/Contents/Resources/root"
mkdir -p "$APP/Contents/MacOS" "$RES"
tar -xzf "$CACHE/$PBS_ASSET" -C "$RES"          # extracts to $RES/python
PY="$RES/python/bin/python3"
[ -x "$PY" ] || fail "extracted runtime has no bin/python3"
"$PY" -m pip install --quiet --upgrade pip
"$PY" -m pip install --quiet --no-cache-dir "${ASR_PACKAGES[@]}"
ok "installed ${ASR_PACKAGES[*]}"
CMAKE_ARGS="-DGGML_METAL=on" "$PY" -m pip install --quiet --no-cache-dir "$LLM_PACKAGE"
ok "installed $LLM_PACKAGE with Metal"
"$PY" -c "import mlx.core, parakeet_mlx, llama_cpp, huggingface_hub; print('imports ok')"
# Trim what no sidecar needs.
rm -rf "$RES/python/lib/python3.12/test" "$RES/python/lib/python3.12/idlelib"
find "$RES/python" -name "__pycache__" -type d -prune -exec rm -rf {} +

bold "assemble bundle"
cp -R "$ROOT/services" "$RES/services"
cp -R "$ROOT/src" "$RES/src"
find "$RES/services" "$RES/src" -name "__pycache__" -type d -prune -exec rm -rf {} +
for venv in .venv-nemotron-mac .venv-llm-polish-mac; do
    mkdir -p "$RES/$venv/bin"
    ln -s "../../python/bin/python3" "$RES/$venv/bin/python"
done
cp "$ROOT/target/release/sunoto-daemon" "$APP/Contents/MacOS/sunoto-daemon"
cp "$ROOT/target/release/sunoto-overlay" "$APP/Contents/MacOS/sunoto-overlay"
"$ROOT/target/release/sunoto-daemon" setup --print-plist > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null
# Use the machine-local identity when this build host has one. Release CI
# stays ad-hoc; setup re-signs the installed copy with the user's identity.
identity="$(security find-identity -v -p codesigning 2>/dev/null \
    | awk '/"Sunoto Local Code Signing"/ { print $2; exit }')"
if [ -n "$identity" ]; then
    codesign --force --deep --sign "$identity" --identifier com.earendil-works.sunoto "$APP" 2>/dev/null
else
    codesign --force --deep --sign - --identifier com.earendil-works.sunoto "$APP" 2>/dev/null
fi
codesign --verify --deep --strict "$APP"
ok "$APP"

bold "smoke test"
"$APP/Contents/MacOS/sunoto-daemon" setup --print-plist >/dev/null
SUNOTO_ROOT="$RES" "$RES/.venv-nemotron-mac/bin/python" -c "import parakeet_mlx, llama_cpp" \
    || fail "venv-shaped symlinks do not resolve"
ok "bundle resolves its own runtime"

bold "archive"
rm -f "$ZIP" "$ZIP.sha256"
ditto -c -k --keepParent "$APP" "$ZIP"
(cd "$OUT" && shasum -a 256 "$(basename "$ZIP")" > "$ZIP.sha256")
ok "$ZIP ($(du -h "$ZIP" | cut -f1))"
cat "$ZIP.sha256"
