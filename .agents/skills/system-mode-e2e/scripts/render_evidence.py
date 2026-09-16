#!/usr/bin/env python3
"""Render auditable PNG evidence snapshots from an E2E artifact and redacted log."""

from __future__ import annotations

import argparse
import json
import textwrap
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


WIDTH, HEIGHT = 1440, 900
BACKGROUND = "#0b1020"
PANEL = "#141b31"
BORDER = "#283453"
TEXT = "#e8ecf8"
MUTED = "#9da9c7"
GREEN = "#67e8a3"
RED = "#ff7a90"
YELLOW = "#f4c96b"


def font(size: int, mono: bool = False) -> ImageFont.FreeTypeFont:
    path = "/System/Library/Fonts/Monaco.ttf" if mono else "/System/Library/Fonts/SFNS.ttf"
    return ImageFont.truetype(path, size)


def panel(draw: ImageDraw.ImageDraw, box: tuple[int, int, int, int], radius: int = 18) -> None:
    draw.rounded_rectangle(box, radius=radius, fill=PANEL, outline=BORDER, width=2)


def save_summary(run: Path, artifact: dict[str, object]) -> Path:
    image = Image.new("RGB", (WIDTH, HEIGHT), BACKGROUND)
    draw = ImageDraw.Draw(image)
    draw.text((70, 54), "SUNOTO · SYSTEM MODE E2E", fill=MUTED, font=font(18))
    draw.text((70, 90), "Verification result", fill=TEXT, font=font(44))
    host = artifact["host"]
    draw.text(
        (70, 150),
        f"{host['platform']} {host['release']} · {host['machine']} · {artifact['generated_at']}",
        fill=MUTED,
        font=font(17),
    )
    counts = artifact["counts"]
    for index, (label, color) in enumerate((("PASS", GREEN), ("FAIL", RED), ("SKIP", YELLOW))):
        left = 70 + index * 300
        panel(draw, (left, 205, left + 270, 335))
        draw.text((left + 24, 226), str(counts[label]), fill=color, font=font(48))
        draw.text((left + 24, 292), label.lower(), fill=MUTED, font=font(18))
    panel(draw, (70, 380, 1370, 820))
    draw.text((98, 410), "Passed checks", fill=TEXT, font=font(26))
    y = 460
    checks = [check for check in artifact["checks"] if check["status"] == "PASS"]
    for check in checks[:14]:
        draw.ellipse((100, y + 5, 112, y + 17), fill=GREEN)
        draw.text((128, y), f"{check['mode']} · {check['name']}", fill=TEXT, font=font(17))
        draw.text((950, y), f"{check['duration_ms']} ms", fill=MUTED, font=font(15))
        y += 25
    output = run / "screenshots/result-summary.png"
    output.parent.mkdir(exist_ok=True)
    image.save(output)
    return output


def save_daemon_trace(run: Path) -> Path:
    source = run / "logs/daemon-mock-product-path.log"
    lines = source.read_text(encoding="utf-8", errors="replace").splitlines()
    selected = [
        line
        for line in lines
        if any(
            marker in line
            for marker in (
                "ASR sidecar ready",
                "recording (system)",
                "sent to ASR",
                "System transcript captured",
                "System route completed",
                "presented 1 System suggestion",
                "System suggestions dismissed",
                "shutting down",
            )
        )
    ]
    image = Image.new("RGB", (WIDTH, HEIGHT), BACKGROUND)
    draw = ImageDraw.Draw(image)
    draw.text((70, 54), "MOCK DAEMON PRODUCT PATH", fill=MUTED, font=font(18))
    draw.text((70, 90), "Transcript → worker → palette → cancel", fill=TEXT, font=font(38))
    stages = ["Mock ASR", "target.find", "Palette", "Cancel", "Cleanup"]
    for index, stage in enumerate(stages):
        left = 70 + index * 264
        panel(draw, (left, 170, left + 224, 250), 14)
        draw.text((left + 20, 195), stage, fill=GREEN, font=font(20))
        if index < len(stages) - 1:
            draw.text((left + 232, 195), "→", fill=MUTED, font=font(22))
    panel(draw, (70, 300, 1370, 820))
    draw.text((98, 328), "Redacted daemon evidence", fill=TEXT, font=font(24))
    y = 382
    mono = font(16, mono=True)
    for line in selected:
        for wrapped in textwrap.wrap(line, width=118) or [""]:
            draw.text((100, y), wrapped, fill="#c7d2f0", font=mono)
            y += 30
    output = run / "screenshots/daemon-product-path.png"
    output.parent.mkdir(exist_ok=True)
    image.save(output)
    return output


def embed(run: Path) -> None:
    report = run / "report.html"
    text = report.read_text(encoding="utf-8")
    gallery = """<section><h2>Evidence snapshots</h2><p class='meta'>Rendered deterministically from the machine artifact and redacted daemon log.</p><div class='gallery'><figure><img src='screenshots/result-summary.png' alt='E2E result summary'><figcaption>Result summary</figcaption></figure><figure><img src='screenshots/daemon-product-path.png' alt='Mock daemon product path evidence'><figcaption>Mock daemon product path</figcaption></figure></div></section>"""
    text = text.replace("</style>", ".gallery{display:grid;grid-template-columns:1fr 1fr;gap:18px}.gallery img{width:100%;border:1px solid #283453;border-radius:12px}.gallery figure{margin:0}.gallery figcaption{color:#9da9c7;margin-top:7px}@media(max-width:900px){.gallery{grid-template-columns:1fr}}</style>")
    text = text.replace("<section><h2>Artifacts</h2>", gallery + "<section><h2>Artifacts</h2>")
    report.write_text(text, encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    args = parser.parse_args()
    run = args.run.resolve()
    artifact = json.loads((run / "artifact.json").read_text(encoding="utf-8"))
    save_summary(run, artifact)
    save_daemon_trace(run)
    embed(run)
    print(run / "report.html")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
