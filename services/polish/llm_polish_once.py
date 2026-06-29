#!/usr/bin/env python3
"""One-shot local LLM polish helper for controlled Sunoto daemon trials.

Reads one transcript from stdin and writes one JSON object to stdout. The
daemon treats this helper as advisory and falls back to deterministic polish
whenever the helper fails, times out, returns empty text, or emits hard
validation flags.
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
from collections import Counter
from pathlib import Path

from llama_cpp import Llama


FULL_SYSTEM_PROMPT = (
    "Edit literally as a dictation de-noiser. Remove fillers, accidental repeats, "
    "and superseded attempts. If a phrase is followed by a restart or correction "
    "cue (no wait, I mean, sorry, strike that, never mind, start over, let me "
    "explain), delete that earlier attempt, not the later wording. For "
    "numbers/codes, when a prefix is repeated after um/uh, keep one complete "
    "final sequence. Never change facts, names, spoken digits/dot/at, code, "
    "emails, URLs, negation, or meaningful Wait/Actually. Output only cleaned "
    "transcript."
)

MINIMAL_SYSTEM_PROMPT = (
    "Edit literally as a dictation de-noiser. Remove fillers, accidental repeats, "
    "and superseded attempts. Preserve facts, names, digits, emails, URLs, code, "
    "negation, and meaningful Wait/Actually. If no edit is needed, output exactly "
    "UNCHANGED. If an edit is needed, output exactly EDITED: followed by the "
    "cleaned transcript. Never output EDITED when the cleaned transcript would be "
    "identical to the input. No explanations."
)

CONSTRAINED_SYSTEM_PROMPT = (
    "You merge mid-utterance self-corrections in dictation transcripts. "
    "You change NOTHING else.\n\n"
    "A self-correction is when the speaker retracts an earlier phrase and "
    "re-speaks it differently, signaled mid-utterance by 'actually', 'i mean', "
    "'no wait', 'sorry', 'or rather', 'let me rephrase', 'scratch that', or "
    "'never mind'. The retracted phrase comes BEFORE the cue; the correction "
    "comes AFTER. Delete ONLY the retracted phrase and its cue; keep the "
    "correction and every other word VERBATIM, including small words like "
    "'him', 'her', 'the', 'at'.\n\n"
    "Do NOT remove fillers (already removed). Do NOT fix grammar, tense, word "
    "order, punctuation, capitalization, word choice, or style. Do NOT improve "
    "already-clean text. Do NOT reformat spoken digits, codes, emails, URLs, "
    "phone numbers, or account info; keep them literally as spoken.\n\n"
    "'Actually,' or 'Wait,' at the very START of the utterance is emphasis, "
    "NOT a correction; there is nothing before it to retract, so output OK.\n\n"
    "If there is no mid-utterance self-correction, output exactly OK. If there "
    "is one, output exactly EDIT: followed by the merged transcript, with only "
    "the retracted phrase and its cue removed. Never output EDIT when the "
    "result would be identical to the input. No explanations."
)

DECISION_SYSTEM_PROMPT = (
    "Classify if this transcript needs literal cleanup. OK means already clean, "
    "including facts, numbers, emails, URLs, and code-like text. EDIT means "
    "fillers, repeats, false starts, correction cues, or awkward ASR word order "
    "need cleanup. Output exactly OK or EDIT. No other text."
)

REWRITE_SYSTEM_PROMPT = (
    "Edit literally as a dictation de-noiser. Remove fillers, accidental repeats, "
    "and superseded attempts. Preserve facts, names, spoken digits/dot/at, code, "
    "emails, URLs, negation, and meaningful Wait/Actually. Output only the cleaned "
    "transcript. No explanations."
)

FULL_REPAIR_FEW_SHOT = (
    (
        "My account number is four seven two, um, four seven two nine three one.",
        "My account number is four seven two nine three one.",
    ),
    (
        "Her email is jane, no, janet dot smith at example dot com.",
        "Her email is janet dot smith at example dot com.",
    ),
)

MINIMAL_REPAIR_FEW_SHOT = (
    (
        "The responses are currently really slow.",
        "UNCHANGED",
    ),
    (
        "We should keep the current architecture so latency measurements stay realistic.",
        "UNCHANGED",
    ),
    (
        "The command is cargo test workspace.",
        "UNCHANGED",
    ),
    (
        "Her email is jane, no, janet dot smith at example dot com.",
        "EDITED: Her email is janet dot smith at example dot com.",
    ),
)

CONSTRAINED_REPAIR_FEW_SHOT = (
    (
        "The quarterly review covers all the metrics that the team collected "
        "during the last sprint before we shipped the release candidate.",
        "OK",
    ),
    (
        "I pushed the patch to git hub and updated the read me file.",
        "OK",
    ),
    (
        "Actually, the build passed on the first try this morning.",
        "OK",
    ),
    (
        "He send the report to the leads every friday.",
        "OK",
    ),
    (
        "The order number is seven four two nine zero one.",
        "OK",
    ),
    (
        "The dashboard metrics look healthy today.",
        "OK",
    ),
    (
        "Please open settings, no wait, open the dashboard.",
        "EDIT: Please open the dashboard.",
    ),
    (
        "Her email is jane, no, janet dot smith at example dot com.",
        "EDIT: Her email is janet dot smith at example dot com.",
    ),
    (
        "Meet her at the cafe, actually, at the library tomorrow.",
        "EDIT: Meet her at the library tomorrow.",
    ),
    (
        "Ship it on monday, i mean tuesday, i mean wednesday.",
        "EDIT: Ship it on wednesday.",
    ),
)

DECISION_FEW_SHOT = (
    (
        "The responses are currently really slow.",
        "OK",
    ),
    (
        "When you run the experiment, use the current architecture so the latency is realistic.",
        "OK",
    ),
    (
        "The account number is four seven two nine three one.",
        "OK",
    ),
    (
        "Her email is jane dot smith at example dot com.",
        "OK",
    ),
    (
        "Run cargo test dash dash workspace before pushing.",
        "OK",
    ),
    (
        "I want to, let me get the file.",
        "EDIT",
    ),
    (
        "Could you, I will do it myself.",
        "EDIT",
    ),
    (
        "The meeting, I mean, the call is at noon.",
        "EDIT",
    ),
    (
        "Her email is jane, no, janet dot smith at example dot com.",
        "EDIT",
    ),
    (
        "I think we should test our model how it works.",
        "EDIT",
    ),
)

REWRITE_FEW_SHOT = (
    (
        "My account number is four seven two, um, four seven two nine three one.",
        "My account number is four seven two nine three one.",
    ),
    (
        "Her email is jane, no, janet dot smith at example dot com.",
        "Her email is janet dot smith at example dot com.",
    ),
    (
        "The meeting, I mean, the call is at noon.",
        "The call is at noon.",
    ),
    (
        "My account number is four seven two four seven two nine three one.",
        "My account number is four seven two nine three one.",
    ),
    (
        "Please send this to Rahul, um, please send this to Priya tomorrow.",
        "Please send this to Priya tomorrow.",
    ),
)

# Backwards-compatible names for older imports/tests. New code should call
# system_prompt()/repair_few_shot() so the runtime output mode is honored.
SYSTEM_PROMPT = FULL_SYSTEM_PROMPT
REPAIR_FEW_SHOT = FULL_REPAIR_FEW_SHOT

MODEL_RELATIVE = (
    "models/llm-polish-hf/gemma-4-e2b-it-q4/"
    "google_gemma-4-E2B-it-Q4_K_M.gguf"
)

EXPLICIT_CORRECTION_MARKERS = (
    "actually",
    "i mean",
    "no wait",
    "wait no",
    "scratch that",
    "strike that",
)

NEGATIONS = ("no", "not", "never", "without")
CONTENT_DROP_STOPWORDS = {
    "a",
    "an",
    "and",
    "are",
    "as",
    "at",
    "be",
    "but",
    "by",
    "for",
    "from",
    "how",
    "i",
    "in",
    "is",
    "it",
    "of",
    "on",
    "or",
    "that",
    "the",
    "this",
    "to",
    "was",
    "we",
    "were",
    "with",
    "you",
}
FILLER_CUE_WORDS = {"um", "uh", "erm", "er"}
MEANINGFUL_LEADING_MARKERS = ("actually", "wait")
DIGIT_WORDS = {
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "thousand",
}


def repo_root() -> Path:
    if os.environ.get("SUNOTO_ROOT"):
        return Path(os.environ["SUNOTO_ROOT"]).resolve()
    return Path(__file__).resolve().parents[2]


def model_path() -> Path:
    if os.environ.get("SUNOTO_LLM_POLISH_MODEL_PATH"):
        return Path(os.environ["SUNOTO_LLM_POLISH_MODEL_PATH"]).expanduser().resolve()
    return repo_root() / MODEL_RELATIVE


def model_label(path: Path | None = None) -> str:
    path = path or model_path()
    if path.suffix == ".gguf":
        return path.stem
    return path.name


def output_mode() -> str:
    mode = os.environ.get("SUNOTO_LLM_POLISH_OUTPUT_MODE", "minimal").strip().lower()
    if mode not in {"minimal", "full"}:
        return "minimal"
    return mode


def polish_mode() -> str:
    mode = os.environ.get("SUNOTO_LLM_POLISH_MODE", "one_pass_minimal").strip().lower()
    aliases = {
        "constrained": "constrained_one_call",
        "one_pass_constrained": "constrained_one_call",
        "one-pass-constrained": "constrained_one_call",
        "constrained-one-call": "constrained_one_call",
        "minimal": "one_pass_minimal",
        "one_pass": "one_pass_minimal",
        "one-pass": "one_pass_minimal",
        "one-pass-minimal": "one_pass_minimal",
        "two-step": "two_step",
    }
    mode = aliases.get(mode, mode)
    if mode not in {"one_pass_minimal", "two_step", "constrained_one_call"}:
        return "one_pass_minimal"
    return mode


def system_prompt(mode: str | None = None) -> str:
    return MINIMAL_SYSTEM_PROMPT if (mode or output_mode()) == "minimal" else FULL_SYSTEM_PROMPT


def repair_few_shot(mode: str | None = None) -> tuple[tuple[str, str], ...]:
    return MINIMAL_REPAIR_FEW_SHOT if (mode or output_mode()) == "minimal" else FULL_REPAIR_FEW_SHOT


def messages_for(transcript: str, mode: str | None = None) -> list[dict[str, str]]:
    active_mode = mode or output_mode()
    messages = [{"role": "system", "content": system_prompt(active_mode)}]
    for input_text, output_text in repair_few_shot(active_mode):
        messages.append({"role": "user", "content": f"Clean this transcript:\n{input_text}"})
        messages.append({"role": "assistant", "content": output_text})
    messages.append({"role": "user", "content": f"Clean this transcript:\n{transcript}"})
    return messages


def _read_text_env_or_file(value_env: str, file_env: str) -> str | None:
    path = os.environ.get(file_env)
    if path:
        try:
            return Path(path).expanduser().read_text().strip()
        except OSError as error:
            print(f"[llm-polish] failed to read {file_env}={path}: {error}", file=sys.stderr)
            return None
    value = os.environ.get(value_env)
    if value is not None and value.strip():
        return value.strip()
    return None


def constrained_system_prompt() -> str:
    return (
        _read_text_env_or_file(
            "SUNOTO_LLM_POLISH_CONSTRAINED_SYSTEM_PROMPT",
            "SUNOTO_LLM_POLISH_CONSTRAINED_SYSTEM_PROMPT_FILE",
        )
        or CONSTRAINED_SYSTEM_PROMPT
    )


def constrained_repair_few_shot() -> tuple[tuple[str, str], ...]:
    raw = _read_text_env_or_file(
        "SUNOTO_LLM_POLISH_CONSTRAINED_FEW_SHOT_JSON",
        "SUNOTO_LLM_POLISH_CONSTRAINED_FEW_SHOT_FILE",
    )
    if raw is None:
        return CONSTRAINED_REPAIR_FEW_SHOT

    try:
        data = json.loads(raw)
        if not isinstance(data, list):
            raise ValueError("few-shot override must be a JSON list")
        pairs: list[tuple[str, str]] = []
        for item in data:
            if isinstance(item, dict):
                input_text = item.get("input")
                output_text = item.get("output")
            elif isinstance(item, list) and len(item) == 2:
                input_text, output_text = item
            else:
                raise ValueError("few-shot item must be [input, output] or {input, output}")
            if not isinstance(input_text, str) or not isinstance(output_text, str):
                raise ValueError("few-shot input/output must be strings")
            pairs.append((input_text, output_text))
        return tuple(pairs)
    except (json.JSONDecodeError, ValueError) as error:
        print(f"[llm-polish] invalid constrained few-shot override: {error}", file=sys.stderr)
        return CONSTRAINED_REPAIR_FEW_SHOT


def constrained_messages_for(transcript: str) -> list[dict[str, str]]:
    messages = [{"role": "system", "content": constrained_system_prompt()}]
    for input_text, output_text in constrained_repair_few_shot():
        messages.append({"role": "user", "content": f"Clean this transcript:\n{input_text}"})
        messages.append({"role": "assistant", "content": output_text})
    messages.append({"role": "user", "content": f"Clean this transcript:\n{transcript}"})
    return messages


def decision_messages_for(transcript: str) -> list[dict[str, str]]:
    messages = [{"role": "system", "content": DECISION_SYSTEM_PROMPT}]
    for input_text, output_text in DECISION_FEW_SHOT:
        messages.append({"role": "user", "content": f"Transcript:\n{input_text}"})
        messages.append({"role": "assistant", "content": output_text})
    messages.append({"role": "user", "content": f"Transcript:\n{transcript}"})
    return messages


def rewrite_messages_for(transcript: str) -> list[dict[str, str]]:
    messages = [{"role": "system", "content": REWRITE_SYSTEM_PROMPT}]
    for input_text, output_text in REWRITE_FEW_SHOT:
        messages.append({"role": "user", "content": f"Clean this transcript:\n{input_text}"})
        messages.append({"role": "assistant", "content": output_text})
    messages.append({"role": "user", "content": f"Clean this transcript:\n{transcript}"})
    return messages


def word_tokens(text: str) -> list[str]:
    return re.findall(r"[A-Za-z0-9]+(?:[-'][A-Za-z0-9]+)?", text.lower())


def dynamic_tokens(text: str, mode: str | None = None) -> int:
    words = len(word_tokens(text))
    if (mode or output_mode()) == "minimal":
        return min(80, max(8, words + 24))
    return min(96, max(24, 2 * words + 16))


def decision_max_tokens() -> int:
    try:
        return max(1, int(os.environ.get("SUNOTO_LLM_POLISH_DECISION_MAX_TOKENS", "2")))
    except ValueError:
        return 2


def constrained_max_tokens(text: str) -> int:
    words = len(word_tokens(text))
    return min(88, max(4, words + 26))


def strip_output_wrappers(raw: str) -> str:
    text = raw.strip()
    fence = re.search(r"```(?:text)?\s*(.*?)```", text, flags=re.S | re.I)
    if fence:
        text = fence.group(1).strip()
    return text.strip().strip('"`')


def clean_output(raw: str) -> str:
    text = strip_output_wrappers(raw)
    for prefix in ("Cleaned transcript:", "Cleaned:", "Output:", "Transcript:"):
        if text.lower().startswith(prefix.lower()):
            text = text[len(prefix) :].strip()
    return text.strip().strip('"`')


def clean_model_output(raw: str, transcript: str, mode: str | None = None) -> str:
    if (mode or output_mode()) != "minimal":
        return clean_output(raw)

    text = strip_output_wrappers(raw)
    first_line = text.splitlines()[0].strip() if text.splitlines() else ""
    if re.fullmatch(r"(?i)unchanged[\s.!:]*", first_line) and len(word_tokens(text)) <= 3:
        return transcript

    edited = re.match(r"(?is)^\s*edited\s*:\s*(.+?)\s*$", text)
    if edited:
        return clean_output(edited.group(1))

    edited_block = re.match(r"(?is)^\s*edited\s*\n(.+?)\s*$", text)
    if edited_block:
        return clean_output(edited_block.group(1))

    return clean_output(text)


def clean_constrained_output(raw: str, transcript: str) -> tuple[str, str, bool]:
    text = strip_output_wrappers(raw)
    first_line = text.splitlines()[0].strip() if text.splitlines() else ""
    if re.fullmatch(r"(?i)ok[\s.!:]*", first_line):
        return transcript, "OK", False

    edited = re.match(r"(?is)^\s*edit\s*:\s*(.+?)\s*$", text)
    if edited:
        cleaned = clean_output(edited.group(1))
        if word_tokens(cleaned) == word_tokens(transcript):
            return transcript, "OK", False
        return cleaned, "EDIT", False

    return clean_output(text), "MALFORMED", True


def parse_decision_output(raw: str) -> str | None:
    text = strip_output_wrappers(raw).strip().upper()
    text = re.sub(r"[\s.!:]+$", "", text)
    if text in {"OK", "KEEP", "UNCHANGED"}:
        return "UNCHANGED"
    if text == "EDIT":
        return "EDIT"
    return None


def contains_explicit_correction(text: str) -> bool:
    haystack = " ".join(word_tokens(text))
    return any(marker in haystack for marker in EXPLICIT_CORRECTION_MARKERS)


def contains_plain_no_correction(text: str) -> bool:
    return bool(re.search(r"\b[A-Za-z0-9][^.!?]*,\s*no,\s*(?!that\b|this\b|what\b)", text, re.I))


def contains_no_correction(text: str) -> bool:
    haystack = " ".join(word_tokens(text))
    return "no wait" in haystack or "wait no" in haystack or contains_plain_no_correction(text)


def has_spoken_digits(text: str) -> bool:
    return any(token in DIGIT_WORDS for token in word_tokens(text))


def introduces_ascii_digit_compaction(raw: str, output: str) -> bool:
    if not has_spoken_digits(raw):
        return False
    return bool(re.search(r"\d", output)) and not bool(re.search(r"\d", raw))


def introduces_formatted_target(raw: str, output: str) -> bool:
    checks = (
        r"\b\S+@\S+\.\S+\b",
        r"https?://\S+|www\.\S+",
        r"\b\d{1,3}(?:\.\d{1,3}){3}\b",
    )
    return any(re.search(pattern, output) and not re.search(pattern, raw) for pattern in checks)


def introduces_spoken_marker_formatting(raw: str, output: str) -> bool:
    raw_tokens = set(word_tokens(raw))
    if "at" in raw_tokens and "@" in output and "@" not in raw:
        return True
    if "dot" in raw_tokens:
        raw_dotted = set(re.findall(r"\b[\w-]+\.[\w.-]+\b", raw))
        output_dotted = set(re.findall(r"\b[\w-]+\.[\w.-]+\b", output))
        if output_dotted - raw_dotted:
            return True
    return False


def introduces_code_formatting(raw: str, output: str) -> bool:
    if "`" in output and "`" not in raw:
        return True
    return '"' in output and '"' not in raw and re.search(r"\bprint\b|\bdef\b|\bclass\b", raw, re.I)


def word_count(text: str, word: str) -> int:
    return sum(1 for token in word_tokens(text) if token == word)


def drops_negation_unsafely(raw: str, output: str) -> bool:
    for word in ("not", "never", "without"):
        if word_count(output, word) < word_count(raw, word):
            return True
    if word_count(output, "no") >= word_count(raw, "no"):
        return False
    return not contains_no_correction(raw)


def drops_correction_no(raw: str, output: str) -> bool:
    return (
        word_count(output, "no") < word_count(raw, "no")
        and not drops_negation_unsafely(raw, output)
        and contains_no_correction(raw)
    )


def significant_content_words(text: str) -> list[str]:
    return [
        token
        for token in word_tokens(text)
        if len(token) > 2 and token not in CONTENT_DROP_STOPWORDS
    ]


def drops_content_unsafely(raw: str, output: str) -> bool:
    if contains_explicit_correction(raw) or contains_no_correction(raw):
        return False
    if any(token in FILLER_CUE_WORDS for token in word_tokens(raw)):
        return False

    raw_words = significant_content_words(raw)
    if len(raw_words) < 4:
        return False

    output_counts = Counter(significant_content_words(output))
    missing = 0
    for word in raw_words:
        if output_counts[word] > 0:
            output_counts[word] -= 1
        else:
            missing += 1

    return missing >= 2 and (missing / len(raw_words)) >= 0.35


def drops_meaningful_leading_marker(raw: str, output: str) -> bool:
    raw_text = raw.strip().lower()
    for marker in MEANINGFUL_LEADING_MARKERS:
        if re.match(rf"^{marker}\s*,", raw_text) and word_count(output, marker) < word_count(raw, marker):
            return True
    return False


def validate(raw: str, output: str) -> dict[str, list[str]]:
    hard_unsafe: list[str] = []
    review_flags: list[str] = []
    if not output.strip():
        hard_unsafe.append("empty_output")
    if len(output) > int(len(raw) * 1.25) + 20:
        review_flags.append("too_long")
    if introduces_ascii_digit_compaction(raw, output):
        hard_unsafe.append("digit_compaction")
    if introduces_formatted_target(raw, output):
        hard_unsafe.append("formatted_target")
    if introduces_spoken_marker_formatting(raw, output):
        hard_unsafe.append("spoken_marker_formatting")
    if introduces_code_formatting(raw, output):
        hard_unsafe.append("code_formatting")
    if drops_negation_unsafely(raw, output):
        hard_unsafe.append("negation_dropped")
    elif drops_correction_no(raw, output):
        review_flags.append("correction_no_dropped")
    if drops_meaningful_leading_marker(raw, output):
        hard_unsafe.append("marker_dropped")
    if drops_content_unsafely(raw, output):
        hard_unsafe.append("content_dropped")
    return {"hard_unsafe": hard_unsafe, "review_flags": review_flags}


def main() -> int:
    transcript = sys.stdin.read().strip()
    path = model_path()
    if not transcript:
        print(json.dumps({"text": "", "hard_unsafe": ["empty_output"], "review_flags": []}))
        return 0
    if not path.is_file():
        raise SystemExit(f"model file not found: {path}")

    started = time.time()
    llm = Llama(
        model_path=str(path),
        n_gpu_layers=int(os.environ.get("SUNOTO_LLM_POLISH_GPU_LAYERS", "-1")),
        n_ctx=int(os.environ.get("SUNOTO_LLM_POLISH_CTX", "2048")),
        n_batch=int(os.environ.get("SUNOTO_LLM_POLISH_BATCH", "512")),
        n_ubatch=int(os.environ.get("SUNOTO_LLM_POLISH_UBATCH", "512")),
        n_threads=int(os.environ.get("SUNOTO_LLM_POLISH_THREADS", "8")),
        flash_attn=os.environ.get("SUNOTO_LLM_POLISH_FLASH_ATTN", "1") != "0",
        verbose=False,
        logits_all=False,
        seed=int(os.environ.get("SUNOTO_LLM_POLISH_SEED", "42")),
    )
    mode = output_mode()
    response = llm.create_chat_completion(
        messages=messages_for(transcript, mode),
        max_tokens=dynamic_tokens(transcript, mode),
        temperature=float(os.environ.get("SUNOTO_LLM_POLISH_TEMPERATURE", "0.1")),
        top_k=int(os.environ.get("SUNOTO_LLM_POLISH_TOP_K", "50")),
        top_p=float(os.environ.get("SUNOTO_LLM_POLISH_TOP_P", "0.95")),
        repeat_penalty=float(os.environ.get("SUNOTO_LLM_POLISH_REPEAT_PENALTY", "1.05")),
    )
    raw = response["choices"][0]["message"].get("content") or ""
    text = clean_model_output(raw, transcript, mode)
    validation = validate(transcript, text)
    print(
        json.dumps(
            {
                "text": text,
                "raw_output": raw.strip(),
                "latency_ms": round((time.time() - started) * 1000),
                "output_mode": mode,
                **validation,
            },
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
