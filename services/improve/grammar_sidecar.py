"""Grammar helper for the pill's Improve button.

Runs a small T5 grammar-correction model (Unbabel/gec-t5_small, Apache-2.0)
on MLX, which Sunoto already ships for the speech model, so no PyTorch is
loaded. The model is loaded on the first request (or a `warm`) and unloaded
after `--unload-after` seconds without use; the process exits after
`--exit-after` idle seconds and the daemon starts it again when needed.

NDJSON on stdin/stdout, like the other sidecars:

    -> {"type": "health"}                         <- {"type": "ready", "backend": "grammar"}
    -> {"type": "warm"}                           (loads the model; no reply)
    -> {"type": "improve", "session_id": 7, "text": "..."}
    <- {"type": "improved", "session_id": 7, "text": "...", "ms": 93}
    <- {"type": "error", "session_id": 7, "message": "..."}

Weights come from a local folder (`--model-dir`) or from Hugging Face at a
pinned revision (the official safetensors conversion), through the same
HF cache the speech model uses.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
import threading
import time
from pathlib import Path

DEFAULT_REPO = "Unbabel/gec-t5_small"
# refs/pr/2 on the model repo: Hugging Face's safetensors conversion of the
# same weights (pytorch_model.bin is a pickle; this is plain tensors).
DEFAULT_REVISION = "779c0250987a442162cd4413bc8577c4734dee98"
MODEL_FILES = ("config.json", "tokenizer.json", "model.safetensors")
PREFIX = "gec: "
CACHE_LIMIT_BYTES = 16 * 1024 * 1024
SENTENCE_END = re.compile(r"(?<=[.!?])\s+(?=[A-Z0-9\"'])")
# transformers' clean_up_tokenization, which the reference model output uses.
CLEANUPS = (
    (" .", "."), (" ?", "?"), (" !", "!"), (" ,", ","), (" ' ", "'"),
    (" n't", "n't"), (" 'm", "'m"), (" 's", "'s"), (" 've", "'ve"), (" 're", "'re"),
)


# ---------------------------------------------------------------- the model


def relative_bucket(relative, bidirectional: bool, num_buckets: int, max_distance: int):
    """T5's bucketing of key-minus-query distances (as in transformers)."""
    import mlx.core as mx

    buckets = mx.zeros(relative.shape, dtype=mx.int32)
    if bidirectional:
        num_buckets //= 2
        buckets = buckets + (relative > 0).astype(mx.int32) * num_buckets
        relative = mx.abs(relative)
    else:
        relative = -mx.minimum(relative, mx.zeros_like(relative))
    max_exact = num_buckets // 2
    is_small = relative < max_exact
    scaled = mx.log(mx.maximum(relative, 1).astype(mx.float32) / max_exact)
    large = max_exact + (scaled / math.log(max_distance / max_exact) * (num_buckets - max_exact)).astype(
        mx.int32
    )
    large = mx.minimum(large, num_buckets - 1)
    return buckets + mx.where(is_small, relative, large)


class T5:
    """Inference-only T5 v1.0 with tied embeddings and ReLU feed-forward."""

    def __init__(self, config: dict, weights: dict, dtype):
        import mlx.core as mx

        self.mx = mx
        self.w = {name: value.astype(dtype) for name, value in weights.items()}
        self.d_model = config["d_model"]
        self.heads = config["num_heads"]
        self.d_kv = config["d_kv"]
        self.eps = config.get("layer_norm_epsilon", 1e-6)
        self.buckets = config.get("relative_attention_num_buckets", 32)
        self.max_distance = config.get("relative_attention_max_distance") or 128
        self.enc_layers = config["num_layers"]
        self.dec_layers = config.get("num_decoder_layers") or config["num_layers"]
        self.start = config.get("decoder_start_token_id", 0)
        self.eos = config.get("eos_token_id", 1)
        mx.eval(self.w)

    def norm(self, x, name):
        mx = self.mx
        x32 = x.astype(mx.float32)
        x32 = x32 * mx.rsqrt(mx.mean(x32 * x32, axis=-1, keepdims=True) + self.eps)
        return self.w[name] * x32.astype(x.dtype)

    def linear(self, x, name):
        return x @ self.w[name].T

    def split(self, x):
        batch, length, _ = x.shape
        return x.reshape(batch, length, self.heads, self.d_kv).transpose(0, 2, 1, 3)

    def merge(self, x):
        batch, _, length, _ = x.shape
        return x.transpose(0, 2, 1, 3).reshape(batch, length, self.heads * self.d_kv)

    def position_bias(self, prefix, queries, keys, offset, bidirectional):
        mx = self.mx
        context = mx.arange(offset, offset + queries)[:, None]
        memory = mx.arange(keys)[None, :]
        bucket = relative_bucket(memory - context, bidirectional, self.buckets, self.max_distance)
        values = self.w[f"{prefix}.block.0.layer.0.SelfAttention.relative_attention_bias.weight"][bucket]
        return values.transpose(2, 0, 1)[None]

    def attend(self, q, k, v, bias):
        mx = self.mx
        # T5 does not scale scores by 1/sqrt(d_kv); its init absorbs it.
        scores = (q @ k.transpose(0, 1, 3, 2)).astype(mx.float32)
        if bias is not None:
            scores = scores + bias.astype(mx.float32)
        weights = mx.softmax(scores, axis=-1).astype(v.dtype)
        return self.merge(weights @ v)

    def encode(self, ids):
        mx = self.mx
        x = self.w["shared.weight"][mx.array([ids])]
        bias = self.position_bias("encoder", len(ids), len(ids), 0, True)
        for i in range(self.enc_layers):
            p = f"encoder.block.{i}.layer"
            h = self.norm(x, f"{p}.0.layer_norm.weight")
            a = f"{p}.0.SelfAttention"
            q, k, v = (self.split(self.linear(h, f"{a}.{n}.weight")) for n in "qkv")
            x = x + self.linear(self.attend(q, k, v, bias), f"{a}.o.weight")
            h = self.norm(x, f"{p}.1.layer_norm.weight")
            f = f"{p}.1.DenseReluDense"
            x = x + self.linear(mx.maximum(self.linear(h, f"{f}.wi.weight"), 0), f"{f}.wo.weight")
        return self.norm(x, "encoder.final_layer_norm.weight")

    def generate(self, ids, max_new_tokens: int):
        mx = self.mx
        memory = self.encode(ids)
        cross = []
        for i in range(self.dec_layers):
            a = f"decoder.block.{i}.layer.1.EncDecAttention"
            cross.append(tuple(self.split(self.linear(memory, f"{a}.{n}.weight")) for n in "kv"))
        cache: list[tuple] = [None] * self.dec_layers
        token, output = self.start, []
        for step in range(max_new_tokens):
            x = self.w["shared.weight"][mx.array([[token]])]
            bias = self.position_bias("decoder", 1, step + 1, step, False)
            for i in range(self.dec_layers):
                p = f"decoder.block.{i}.layer"
                h = self.norm(x, f"{p}.0.layer_norm.weight")
                a = f"{p}.0.SelfAttention"
                q, k, v = (self.split(self.linear(h, f"{a}.{n}.weight")) for n in "qkv")
                if cache[i] is not None:
                    k = mx.concatenate([cache[i][0], k], axis=2)
                    v = mx.concatenate([cache[i][1], v], axis=2)
                cache[i] = (k, v)
                x = x + self.linear(self.attend(q, k, v, bias), f"{a}.o.weight")
                h = self.norm(x, f"{p}.1.layer_norm.weight")
                c = f"{p}.1.EncDecAttention"
                q = self.split(self.linear(h, f"{c}.q.weight"))
                x = x + self.linear(self.attend(q, cross[i][0], cross[i][1], None), f"{c}.o.weight")
                h = self.norm(x, f"{p}.2.layer_norm.weight")
                f = f"{p}.2.DenseReluDense"
                x = x + self.linear(mx.maximum(self.linear(h, f"{f}.wi.weight"), 0), f"{f}.wo.weight")
            x = self.norm(x, "decoder.final_layer_norm.weight") * (self.d_model**-0.5)
            token = int(mx.argmax((x[0, -1] @ self.w["shared.weight"].T).astype(mx.float32)).item())
            if token == self.eos:
                break
            output.append(token)
        return output


class Corrector:
    def __init__(self, folder: Path, dtype_name: str):
        import mlx.core as mx
        from tokenizers import Tokenizer

        config = json.loads((folder / "config.json").read_text())
        self.tokenizer = Tokenizer.from_file(str(folder / "tokenizer.json"))
        # The model's tokenizer.json pads every input to 128 tokens. There is
        # one sentence per call and no attention mask, so padding would be
        # attended to as text and wreck the output.
        self.tokenizer.no_padding()
        dtype = {"bfloat16": mx.bfloat16, "float32": mx.float32}[dtype_name]
        # Freed buffers go back to the system instead of MLX's cache: the
        # fp32 originals would otherwise stay resident after the conversion.
        mx.set_cache_limit(CACHE_LIMIT_BYTES)
        self.model = T5(config, mx.load(str(folder / "model.safetensors")), dtype)
        mx.clear_cache()

    def sentence(self, text: str) -> str:
        ids = self.tokenizer.encode(PREFIX + text).ids
        out = self.model.generate(ids, max_new_tokens=min(128, 2 * len(ids) + 16))
        decoded = self.tokenizer.decode(out, skip_special_tokens=True)
        for before, after in CLEANUPS:
            decoded = decoded.replace(before, after)
        return decoded.strip()

    def correct(self, text: str) -> str:
        parts = SENTENCE_END.split(text.strip()) or [text]
        return " ".join(self.sentence(part) for part in parts if part)


def model_folder(args) -> Path:
    if args.model_dir:
        return Path(args.model_dir)
    from huggingface_hub import hf_hub_download

    paths = [hf_hub_download(args.repo, name, revision=args.revision) for name in MODEL_FILES]
    return Path(paths[0]).parent


# ---------------------------------------------------------------- the sidecar


class Server:
    def __init__(self, args):
        self.args = args
        self.lock = threading.Lock()
        self.corrector: Corrector | None = None
        self.last_used = time.monotonic()
        self.out_lock = threading.Lock()

    def emit(self, message: dict) -> None:
        with self.out_lock:
            sys.stdout.write(json.dumps(message) + "\n")
            sys.stdout.flush()

    def log(self, text: str) -> None:
        sys.stderr.write(f"[grammar] {text}\n")
        sys.stderr.flush()

    def ensure_loaded(self) -> Corrector:
        if self.corrector is None:
            started = time.perf_counter()
            self.corrector = Corrector(model_folder(self.args), self.args.dtype)
            # The first call compiles the GPU kernels (~0.8 s); do it now so
            # the click that follows a warm-up is fast.
            self.corrector.correct("Ok.")
            self.log(f"model loaded in {(time.perf_counter() - started) * 1000:.0f} ms")
        return self.corrector

    def handle(self, message: dict) -> None:
        kind = message.get("type")
        if kind == "health":
            self.emit({"type": "ready", "backend": "grammar"})
            return
        with self.lock:
            self.last_used = time.monotonic()
            try:
                if kind == "warm":
                    self.ensure_loaded()
                elif kind == "improve":
                    started = time.perf_counter()
                    text = self.ensure_loaded().correct(str(message.get("text", "")))
                    self.emit({
                        "type": "improved",
                        "session_id": message.get("session_id"),
                        "text": text,
                        "ms": round((time.perf_counter() - started) * 1000),
                    })
                elif kind == "unload":
                    self.corrector = None
                else:
                    raise ValueError(f"unknown request type {kind!r}")
            except Exception as error:  # reported, never fatal
                self.emit({"type": "error", "session_id": message.get("session_id"), "message": str(error)})
            self.last_used = time.monotonic()

    def check_idle(self, now: float) -> bool:
        """Unload the model when idle long enough; True means exit now."""
        with self.lock:
            idle = now - self.last_used
            if self.corrector is not None and idle >= self.args.unload_after:
                self.corrector = None
                self.log(f"model unloaded after {self.args.unload_after:.0f} s idle")
            return self.args.exit_after > 0 and idle >= self.args.exit_after

    def watch_idle(self) -> None:
        while True:
            time.sleep(1.0)
            if self.check_idle(time.monotonic()):
                self.log(f"exiting after {self.args.exit_after:.0f} s idle")
                sys.stdout.flush()
                import os

                os._exit(0)

    def run(self) -> None:
        threading.Thread(target=self.watch_idle, daemon=True).start()
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                self.log(f"ignored non-JSON line: {line[:80]!r}")
                continue
            self.handle(message)


def main() -> None:
    parser = argparse.ArgumentParser(description="Sunoto grammar helper (Improve button)")
    parser.add_argument("--model-dir", help="local folder with config.json, tokenizer.json, model.safetensors")
    parser.add_argument("--repo", default=DEFAULT_REPO)
    parser.add_argument("--revision", default=DEFAULT_REVISION)
    parser.add_argument("--dtype", choices=("bfloat16", "float32"), default="bfloat16")
    parser.add_argument("--unload-after", type=float, default=60.0)
    parser.add_argument("--exit-after", type=float, default=300.0)
    parser.add_argument("--once", help="correct this text, print it, and exit (for testing)")
    args = parser.parse_args()
    if args.once is not None:
        print(Server(args).ensure_loaded().correct(args.once))
        return
    Server(args).run()


if __name__ == "__main__":
    main()
