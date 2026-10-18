#!/usr/bin/env python3
"""Build the persona benchmark (tools/vocab/personas.json) in build/personas/.

For every persona:
  - a pretend home folder with its folders, project files and apps, so
    `sunoto-daemon words --home` builds the lists the daemon would;
  - screen.txt, contacts.txt and recent.txt for the other sources;
  - each sentence spoken by eight macOS voices (US, UK, three Indian,
    Australian, Irish, South African) as 16 kHz WAVs, listed in manifest.json.
"""

from __future__ import annotations

import json
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
OUT = HERE.parents[1] / "build" / "personas"
VOICES = ("Samantha", "Daniel", "Rishi", "Aman", "Tara", "Karen", "Moira", "Tessa")


def speak(job) -> None:
    text, voice, path = job
    if path.exists():
        return
    aiff = path.with_suffix(".aiff")
    subprocess.run(["say", "-v", voice, "-o", str(aiff), text], check=True)
    subprocess.run(
        ["afconvert", "-f", "WAVE", "-d", "LEI16@16000", "-c", "1", str(aiff), str(path)],
        check=True,
    )
    aiff.unlink()


def build_home(name: str, persona: dict) -> Path:
    home = OUT / name / "home"
    for root, folders in persona["folders"].items():
        for folder in folders:
            (home / root / folder).mkdir(parents=True, exist_ok=True)
    project = persona.get("project")
    if project:
        root = home / project["path"]
        (root / ".git").mkdir(parents=True, exist_ok=True)
        for directory in project["dirs"]:
            (root / directory).mkdir(parents=True, exist_ok=True)
        for file in project["files"]:
            (root / file).parent.mkdir(parents=True, exist_ok=True)
            (root / file).touch()
    for app in persona["apps"]:
        (home / "Applications" / f"{app}.app").mkdir(parents=True, exist_ok=True)
    (OUT / name / "screen.txt").write_text(persona["screen"])
    (OUT / name / "contacts.txt").write_text("\n".join(persona["contacts"]))
    (OUT / name / "recent.txt").write_text("\n".join(persona["recent_files"]))
    return home


def main() -> int:
    personas = {k: v for k, v in json.loads((HERE / "personas.json").read_text()).items() if not k.startswith("_")}
    audio = OUT / "audio"
    audio.mkdir(parents=True, exist_ok=True)
    manifest, jobs = [], []
    for name, persona in personas.items():
        home = build_home(name, persona)
        project = persona.get("project")
        for kind in ("names", "ordinary"):
            for i, item in enumerate(persona[kind]):
                item = item if isinstance(item, dict) else {"say": item, "names": [], "in": []}
                for voice in VOICES:
                    path = audio / f"{name}-{kind}-{i:02d}-{voice.lower()}.wav"
                    jobs.append((item["say"], voice, path))
                    manifest.append({
                        "id": f"{name}-{kind}-{i:02d}", "persona": name,
                        "set": "names" if kind == "names" else "ordinary",
                        "voice": voice, "text": item["say"], "names": item["names"],
                        "in": item["in"], "audio": str(path), "home": str(home),
                        "project": str(home / project["path"]) if project else None,
                    })
    with ThreadPoolExecutor(max_workers=8) as pool:
        list(pool.map(speak, jobs))
    (OUT / "manifest.json").write_text(json.dumps(manifest, indent=1))
    print(f"wrote {len(manifest)} clips for {len(personas)} personas to {audio}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
