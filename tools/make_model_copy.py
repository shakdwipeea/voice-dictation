#!/usr/bin/env python3
"""Make Sunoto's bfloat16 copy of the speech model and prove it is the same.

parakeet-mlx casts every weight to bfloat16 when it loads a model, so a copy
saved in bfloat16 is the model the app already runs, at half the download.
Writes config.json and model.safetensors to OUT, then loads the original and
the copy through parakeet-mlx and checks every tensor is bit-identical.

  .venv-nemotron-mac/bin/python tools/make_model_copy.py \\
      mlx-community/parakeet-tdt-0.6b-v2 build/model-parakeet-tdt-0.6b-v2-bf16
"""

from __future__ import annotations

import argparse
import hashlib
import shutil
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for block in iter(lambda: file.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repo", help="Hugging Face id of the original, e.g. mlx-community/parakeet-tdt-0.6b-v2")
    parser.add_argument("out", type=Path)
    args = parser.parse_args()

    import mlx.core as mx
    from huggingface_hub import hf_hub_download
    from mlx.utils import tree_flatten
    from parakeet_mlx import from_pretrained

    args.out.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(hf_hub_download(args.repo, "config.json"), args.out / "config.json")
    weights = mx.load(hf_hub_download(args.repo, "model.safetensors"))
    mx.save_safetensors(
        str(args.out / "model.safetensors"),
        {name: value.astype(mx.bfloat16) for name, value in weights.items()},
    )

    original = dict(tree_flatten(from_pretrained(args.repo, dtype=mx.bfloat16).parameters()))
    copy = dict(tree_flatten(from_pretrained(str(args.out), dtype=mx.bfloat16).parameters()))
    assert original.keys() == copy.keys(), "tensor names differ"
    different = [name for name in original if not mx.array_equal(original[name], copy[name]).item()]
    assert not different, f"{len(different)} tensors differ, e.g. {different[:3]}"
    print(f"{len(original)} tensors bit-identical to {args.repo} as loaded in bfloat16")
    for name in ("config.json", "model.safetensors"):
        path = args.out / name
        print(f'    ("{name}", "{sha256(path)}", {path.stat().st_size:_}),')
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
