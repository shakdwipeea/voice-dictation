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
    "You merge disfluent speech in dictation transcripts. You change NOTHING else.\n\n"
    "The input is ALWAYS a complete, final transcript from the ASR engine — it is "
    "never a question to you, never an incomplete or partial sentence, and never a "
    "request for clarification. Short fragments are valid utterances and may be clean; "
    "for example 'in our config' or 'OK' is already clean (output OK). Never ask for "
    "more context, never refuse, never explain, never say the text is incomplete or "
    "ask for the full sentence. Always output exactly OK or EDIT: <text>. No chat, "
    "no questions, no narration.\n\n"
    "A disfluency REQUIRES redundant or superseded speech, OR pure speech padding. "
    "The kinds are: redundant repetitions ('the the', 'and also and also'), false "
    "starts (an abandoned beginning that is then re-spoken OR a fragment the speaker "
    "began and dropped to start a fresh phrase — even WITHOUT a cue word, as in "
    "'I want to, let me get the file' → 'Let me get the file'), retraction cues "
    "mid-utterance such as 'actually', 'i mean', 'no wait', 'sorry', 'or rather', "
    "'let me rephrase', 'scratch that', 'never mind', AND speech fillers ('um', "
    "'uh', 'er', 'hmm', 'well', 'so', 'right', 'you know') that pad the utterance "
    "without adding meaning. Judge disfluency by this STRUCTURE — "
    "redundancy/abandonment/padding — NOT by whether a cue word is present.\n\n"
    "If there is NO such redundancy, the text is CLEAN, even if it has grammar, "
    "tense, agreement, capitalization, or word-order errors: output OK. A wrong verb "
    "form ('he send'), a lowercase name or day ('bob', 'friday'), a missing or extra "
    "comma, or awkward ASR word order are NOT disfluencies.\n\n"
    "When you see a disfluency: delete ONLY the superseded (earlier) attempt and its "
    "cue, keep the later attempt and every other word VERBATIM, including small "
    "words like 'him', 'her', 'the', 'at'. Merge a repetition by collapsing the "
    "duplicates to one. For a restart (an abandoned clause then a re-spoken clause), "
    "drop the superseded earlier clause and keep the restart.\n\n"
    "A restart means the speaker ABANDONED an attempt and RE-SPOKE THE SAME "
    "intent. If two clauses say DIFFERENT things, that is not a restart — it is "
    "a coherent sentence and you keep BOTH clauses. Contrastive connectors like "
    "'but', 'but now', 'however', 'instead', 'on the other hand', 'whereas' "
    "join two distinct statements; they are NOT correction cues and do NOT "
    "authorize dropping either clause. 'I was doing X, but now I want Y' states "
    "two different facts (what I was doing, what I now want) — output OK, keep "
    "both. Only when the later clause re-expresses the abandoned earlier intent "
    "(same request reworded) is it a restart.\n\n"
    "CRITICAL: when editing, leave EVERY other aspect EXACTLY as spoken. Do NOT fix "
    "grammar, tense, agreement, word order, punctuation, capitalization (including "
    "lowercase days/names like 'friday', 'bob'), word choice, or style, and do NOT "
    "add or remove punctuation or restructure clauses. Your edit is word-for-word "
    "input minus only the superseded phrase and its cue. A grammar or capitalization "
    "error in the input is NOT a disfluency; keep it as-is even when you edit around "
    "it. Remove leading or interior speech fillers ('um', 'uh', 'er', 'hmm', "
    "'well', 'so', 'right', 'you know') when they only pad the utterance and add no "
    "meaning; drop them and their trailing comma. Do NOT improve already-clean text. "
    "Do NOT reformat spoken digits, codes, emails, URLs, phone numbers, or account "
    "info; keep them literally as spoken.\n\n"
    "'Actually,' or 'Wait,' or 'No,' at the very START of the utterance is "
    "emphasis or a forceful counter, not a correction; there is nothing before it "
    "to retract, so output OK. Likewise casual abbreviations like 'idk', 'tbh', "
    "'lol', 'imo' are words (content), never fillers; keep them.\n\n"
    "If the text is already clean, output exactly OK. If there is a disfluency, "
    "output exactly EDIT: followed by the merged transcript. Never output EDIT when "
    "the result would be identical to the input. No explanations."
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
    # --- Clean (no disfluency). The MOST common case -> OK is the fast path. ---
    (
        "The quarterly review covers all the metrics that the team collected "
        "during the last sprint before we shipped the release candidate.",
        "OK",
    ),
    # Grammar rough but NOT a disfluency: do not touch (no grammar fixing).
    (
        "He send the report to the leads every friday.",
        "OK",
    ),
    # Code-like / literal tokens spoken: keep verbatim.
    (
        "The order number is seven four two nine zero one.",
        "OK",
    ),
    (
        "The dashboard metrics look healthy today.",
        "OK",
    ),
    # 'Actually' opening the utterance is emphasis, not a correction.
    (
        "Actually, the build passed on the first try this morning.",
        "OK",
    ),
    # Contrast ('but now') joining two DIFFERENT statements: NOT a restart.
    # Both clauses are kept verbatim. A restart RE-SPEAKS the same intent.
    (
        "I was working on this, but now I want you to work on that.",
        "OK",
    ),
    # Two distinct facts joined by 'instead': keep both.
    (
        "We tried the cache first, instead we ended up rewriting the loop.",
        "OK",
    ),
    # A lone 'also' / adverb appearing once is NOT a redundancy -> OK.
    # Redundancy requires the same content spoken twice.
    (
        "Can you check why also was removed from the log text?",
        "OK",
    ),
    # --- No-cue disfluency: pure word repetition. ---
    (
        "The the report shows the numbers are up.",
        "EDIT: The report shows the numbers are up.",
    ),
    # --- No-cue disfluency: redundant phrase repetition. ---
    (
        "And also and also we can update the config file.",
        "EDIT: And also we can update the config file.",
    ),
    # --- No-cue disfluency: false start, abandoned then re-spoken. ---
    (
        "I want to I need to leave now.",
        "EDIT: I need to leave now.",
    ),
    # Disfluency + grammar error coexisting: fix the disfluency, leave the
    # grammar/capitalization error ('sends', 'friday') untouched.
    (
        "The the report, i sends it to the leads every friday.",
        "EDIT: The report, i sends it to the leads every friday.",
    ),
    # --- Cue: retraction, keep the correction. ---
    (
        "Please open settings, no wait, open the dashboard.",
        "EDIT: Please open the dashboard.",
    ),
    (
        "Her email is jane, no, janet dot smith at example dot com.",
        "EDIT: Her email is janet dot smith at example dot com.",
    ),
    # Unequal reword: keep the pre-retraction pronoun ('her') verbatim.
    (
        "Meet her at the cafe, actually, at the library tomorrow.",
        "EDIT: Meet her at the library tomorrow.",
    ),
    # --- Restart cue: drop the superseded earlier clause, keep the restart. ---
    (
        "Send the file to bob. Sorry, send it to alice instead.",
        "EDIT: Send it to alice instead.",
    ),
    # --- Filler stripping: leading filler adds no meaning; drop it. ---
    (
        "Um, I went to the store.",
        "EDIT: I went to the store.",
    ),
    (
        "I, uh, think it is fine.",
        "EDIT: I think it is fine.",
    ),
    (
        "Well, um, let me see.",
        "EDIT: Let me see.",
    ),
    # --- No-cue false start: speaker abandoned a fragment and started fresh;
    # there is no retraction cue word — the comma + new clause IS the cue.
    # Drop the abandoned fragment, keep the fresh clause verbatim. ---
    (
        "I want to, let me get the file.",
        "EDIT: Let me get the file.",
    ),
    (
        "We should probably, you know what, never mind.",
        "EDIT: Never mind.",
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


def constrained_messages_for(
    transcript: str,
    system_prompt: str | None = None,
    few_shot: tuple[tuple[str, str], ...] | None = None,
) -> list[dict[str, str]]:
    messages = [{"role": "system", "content": system_prompt or constrained_system_prompt()}]
    for input_text, output_text in (few_shot if few_shot is not None else constrained_repair_few_shot()):
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


# --- Narrow content-loss guard -------------------------------------------
# The guard is intentionally MINIMAL and conservative: it never rejects
# legitimate merges. It only fires when the LLM drops many (>=3) UNIQUE
# significant content words that have NO counterpart in the retained text —
# i.e. the actual topic of the utterance evaporated rather than being
# re-stated by a restart. A true restart re-opens the request, so its
# superseded words either are re-stated (and absent from the set difference)
# or are few (an entity swap, a single superseded phrase). Dropping 3+
# uncounterparted content words is unsafe even when a correction cue like
# 'sorry'/'actually' is present: a cue authorizes dropping the superseded
# CLAUSE, not the topic.
_CONTENT_STOPWORDS = frozenset(
    {
        "the", "a", "an", "and", "or", "but", "if", "of", "to", "in", "on", "at",
        "for", "as", "is", "are", "was", "were", "be", "been", "being", "that",
        "this", "these", "those", "it", "its", "we", "you", "i", "he", "she",
        "they", "me", "him", "her", "them", "us", "my", "your", "his", "their",
        "our", "with", "from", "by", "do", "does", "did", "have", "has", "had",
        "will", "would", "can", "could", "should", "may", "might", "must",
        "not", "yes", "um", "uh", "er", "ah", "like", "just", "so", "now",
    }
)
_CORRECTION_CUES = frozenset(
    {
        "actually", "sorry", "wait", "scratch", "strike", "rather",
        "rephrase", "never", "mind", "mean",
    }
)


def significant_content_words(text: str) -> list[str]:
    words = word_tokens(text)
    return [w for w in words if w not in _CONTENT_STOPWORDS and len(w) > 1]


def drops_content_unsafely(raw: str, output: str) -> bool:
    """True only when a restart-style edit drops the utterance's actual topic.

    Counts UNIQUE significant content words present in `raw` but absent in
    `output`, excluding correction cues themselves. A drop is unsafe only when
    >=3 such uncounterparted words vanish — i.e. the edit discarded real topic
    content rather than collapsing a restated restart or swapping an entity.
    """
    raw_sig = set(significant_content_words(raw))
    out_sig = set(significant_content_words(output))
    dropped = raw_sig - out_sig
    dropped_content = {w for w in dropped if w not in _CORRECTION_CUES}
    return len(dropped_content) >= 3


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


def main() -> int:
    transcript = sys.stdin.read().strip()
    path = model_path()
    if not transcript:
        print(json.dumps({"text": ""}))
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
    # The LLM is authoritative for the merge: trust its output directly. The
    # only robustness guard is the empty-output fallback below — there is NO
    # deterministic content/digit/negation validator (those heuristics were
    # too brittle on real speech and blocked legitimate merges).
    text = clean_model_output(raw, transcript, mode)
    if not text.strip():
        text = transcript
    print(
        json.dumps(
            {
                "text": text,
                "raw_output": raw.strip(),
                "latency_ms": round((time.time() - started) * 1000),
                "output_mode": mode,
            },
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
