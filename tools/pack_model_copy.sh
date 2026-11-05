#!/usr/bin/env bash
# Pack Sunoto's copy of the speech model for download. Publish the result next
# to the plain files in the model release, and keep those: versions up to
# 0.1.16 download them, and newer ones fall back to them.
#
#   tools/pack_model_copy.sh DIR     (a folder from tools/make_model_copy.py)
#
# Writes DIR/model.safetensors.tar.xz and prints the size for ASR_MODEL_PACK in
# apps/daemon/src/setup.rs. The weights are 16-bit, which lp=1 and pb=1 tell
# the compressor: about 2 points smaller than plain xz at the same speed. The
# app unpacks with macOS's own tar, so it needs nothing installed. Needs xz
# (brew install xz) on the machine that packs.
set -euo pipefail

DIR="${1:?usage: tools/pack_model_copy.sh DIR}"
PACK="$DIR/model.safetensors.tar.xz"
[ -f "$DIR/model.safetensors" ] || { echo "no model.safetensors in $DIR" >&2; exit 1; }

COPYFILE_DISABLE=1 tar --no-xattrs -cf - -C "$DIR" model.safetensors \
    | xz --lzma2=preset=6,lc=0,lp=1,pb=1 -T0 > "$PACK.tmp"
mv "$PACK.tmp" "$PACK"

printf 'ASR_MODEL_PACK.bytes: %s\n' "$(stat -f %z "$PACK")"
