"""LLM client for the polish eval loop.

Uses the OpenAI-Python library against the NeuralWatt OpenAI-compatible endpoint
(env config), so the same Chat Completions / response_format json_schema path
works across all their models. Lazy client init, exponential-backoff retries,
returns (parsed_object, usage_dict).

Models (per user): glm-5.2-fast (~5s, clean strict JSON) and kimi-k2.6 (~3.3s,
emits fenced JSON, slightly stronger reasoning). gpt-5.5 via OpenAI-direct is
out of quota and too slow; codex runs gpt-5.5 (= slow), not used here.
"""
from __future__ import annotations
import json, os, re, time
from pathlib import Path
from typing import Any

_BASE = "https://api.neuralwatt.com/v1"
DEFAULT_MODEL = os.environ.get("POLISH_JUDGE_MODEL", "glm-5.2-fast")

def _load_env() -> None:
    env = Path(__file__).resolve().parent / ".env"
    if env.exists():
        for line in env.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            os.environ.setdefault(k.strip(), v.strip())

_load_env()

_client = None
def _c():
    global _client
    if _client is None:
        from openai import OpenAI
        _client = OpenAI(base_url=_BASE, api_key=os.environ["NEURALWATTS_API_KEY"])
    return _client

def _strip_fences(text: str) -> str:
    m = re.search(r"```(?:json)?\s*(\{.*\})\s*```", text, re.DOTALL)
    return m.group(1) if m else text.strip()

def complete(messages, schema, schema_name: str, model: str | None = None,
             reasoning_effort: str = "medium", retries: int = 4, timeout: int = 180) -> tuple[dict, dict]:
    """Structured-output call. Returns (parsed_object, usage_dict).

    Note: NeuralWatt models vary in reasoning_effort support; the param is set
    only if the model name suggests it (glm/kimi accept it via extra_body).
    """
    model = model or DEFAULT_MODEL
    extra = {}
    if reasoning_effort and reasoning_effort != "off":
        extra["reasoning_effort"] = reasoning_effort
    body = {"model": model, "messages": messages,
            "response_format": {"type": "json_schema", "json_schema": {
                "name": schema_name, "strict": True, "schema": schema}},
            "timeout": timeout, **extra}
    last = None
    for i in range(retries):
        try:
            resp = _c().chat.completions.create(**body)
            content = resp.choices[0].message.content or ""
            content = _strip_fences(content)
            obj = json.loads(content)
            u = resp.usage
            usage = {"prompt_tokens": u.prompt_tokens, "completion_tokens": u.completion_tokens,
                     "total_tokens": u.total_tokens,
                     "reasoning_tokens": (getattr(u.completion_tokens_details, "reasoning_tokens", None)
                                          if u.completion_tokens_details else None)}
            return obj, usage
        except Exception as e:
            last = e
            w = 2 ** i
            time.sleep(w)
    raise RuntimeError(f"LLM call failed after {retries} retries: {last}")
