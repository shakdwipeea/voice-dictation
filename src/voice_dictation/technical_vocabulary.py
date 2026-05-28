"""Technical vocabulary hints and conservative transcript cleanup."""
from __future__ import annotations

import os
import re
from pathlib import Path


MAX_VOCABULARY_TERMS = 120

BUILTIN_VOCABULARY = [
    "Git",
    "GitHub",
    "GitLab",
    "origin/master",
    "origin/main",
    "main branch",
    "master branch",
    "pull request",
    "merge conflict",
    "commit hash",
    "git checkout",
    "git clone",
    "git commit",
    "git diff",
    "git fetch",
    "git merge",
    "git pull",
    "git push",
    "git rebase",
    "git status",
    "cherry-pick",
    "Python",
    "JavaScript",
    "TypeScript",
    "JSON",
    "YAML",
    "TOML",
    "HTML",
    "CSS",
    "regex",
    "API",
    "CLI",
    "stdout",
    "stderr",
    "stdin",
    "localhost",
    "OAuth",
    "Docker",
    "Dockerfile",
    "Kubernetes",
    "kubectl",
    "PostgreSQL",
    "SQLite",
    "Redis",
    "Nginx",
    "systemd",
    "Wayland",
    "Hyprland",
    "CTranslate2",
    "faster-whisper",
    "Whisper",
]

_TECHNICAL_CORRECTIONS: list[tuple[re.Pattern[str], str]] = [
    (
        re.compile(
            r"\borigin\s+(?:forward\s+slash|slash|/)\s+"
            r"(?:master|masters|monster|monsters)\b",
            re.IGNORECASE,
        ),
        "origin/master",
    ),
    (
        re.compile(
            r"\borigin\s+(?:forward\s+slash|slash|/)\s+(?:main|maine)\b",
            re.IGNORECASE,
        ),
        "origin/main",
    ),
    (re.compile(r"\b(?:get|good)\s+checkout\b", re.IGNORECASE), "git checkout"),
    (re.compile(r"\b(?:get|good)\s+rebase\b", re.IGNORECASE), "git rebase"),
    (re.compile(r"\b(?:get|good)\s+status\b", re.IGNORECASE), "git status"),
    (re.compile(r"\b(?:get|good)\s+commit\b", re.IGNORECASE), "git commit"),
    (re.compile(r"\b(?:get|good)\s+diff\b", re.IGNORECASE), "git diff"),
    (re.compile(r"\b(?:get|good)\s+pull\b", re.IGNORECASE), "git pull"),
    (re.compile(r"\b(?:get|good)\s+push\b", re.IGNORECASE), "git push"),
    (re.compile(r"\b(?:cube|queue|q)\s+(?:cuddle|cuttle|ctl|control)\b", re.IGNORECASE), "kubectl"),
    (re.compile(r"\bkube\s+(?:cuddle|cuttle|ctl|control)\b", re.IGNORECASE), "kubectl"),
    (re.compile(r"\bkuber\s+net\s+ease\b", re.IGNORECASE), "Kubernetes"),
    (re.compile(r"\bkuber\s+netes\b", re.IGNORECASE), "Kubernetes"),
    (re.compile(r"\btype\s+script\b", re.IGNORECASE), "TypeScript"),
    (re.compile(r"\bjava\s+script\b", re.IGNORECASE), "JavaScript"),
    (re.compile(r"\bpost\s+grass\s+q\s*l\b", re.IGNORECASE), "PostgreSQL"),
    (re.compile(r"\bpost\s+gres\s+q\s*l\b", re.IGNORECASE), "PostgreSQL"),
    (re.compile(r"\bs\s+q\s+lite\b", re.IGNORECASE), "SQLite"),
    (re.compile(r"\bdocker\s+file\b", re.IGNORECASE), "Dockerfile"),
    (re.compile(r"\bstandard\s+out\b", re.IGNORECASE), "stdout"),
    (re.compile(r"\bstandard\s+error\b", re.IGNORECASE), "stderr"),
    (re.compile(r"\blocal\s+host\b", re.IGNORECASE), "localhost"),
    (re.compile(r"\bpull\s+requests\b", re.IGNORECASE), "pull requests"),
]


def default_vocabulary_path() -> Path:
    config_home = os.environ.get("XDG_CONFIG_HOME")
    root = Path(config_home) if config_home else Path.home() / ".config"
    return root / "voice-dictation" / "vocabulary.txt"


def load_vocabulary_file(path: Path) -> list[str]:
    terms: list[str] = []
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except FileNotFoundError:
        return terms
    for line in lines:
        term = line.split("#", 1)[0].strip()
        if term:
            terms.append(term)
    return terms


def build_vocabulary(vocabulary_file: Path | None = None, *, max_terms: int = MAX_VOCABULARY_TERMS) -> list[str]:
    terms = list(BUILTIN_VOCABULARY)
    path = vocabulary_file if vocabulary_file is not None else default_vocabulary_path()
    terms.extend(load_vocabulary_file(path))

    deduped: list[str] = []
    seen: set[str] = set()
    for term in terms:
        key = re.sub(r"\s+", " ", term).strip().casefold()
        if not key or key in seen:
            continue
        seen.add(key)
        deduped.append(term.strip())
        if len(deduped) >= max_terms:
            break
    return deduped


def build_whisper_context(terms: list[str]) -> tuple[str | None, str | None]:
    if not terms:
        return None, None
    glossary = ", ".join(terms)
    return f"Technical vocabulary: {glossary}.", glossary


def apply_technical_corrections(text: str) -> str:
    corrected = text
    for pattern, replacement in _TECHNICAL_CORRECTIONS:
        corrected = pattern.sub(replacement, corrected)
    return re.sub(r"\s+", " ", corrected).strip()
