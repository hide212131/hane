#!/usr/bin/env python3
"""Create deterministic, disposable fixtures for Hane issue #23."""

from __future__ import annotations

import argparse
import json
import shutil
from pathlib import Path


NOTE_BYTES = 1_024
FOLDER_COUNTS = {"folder_1k": 1_000, "folder_10k": 10_000}


def note_bytes(number: int) -> bytes:
    header = f"# Issue 23 note {number:05d}\n\n".encode("ascii")
    paragraph = (b"Deterministic Markdown fixture for repeatable Hane RSS measurements. " * 32)
    body = (paragraph * ((NOTE_BYTES - len(header)) // len(paragraph) + 1))[: NOTE_BYTES - len(header)]
    return header + body


def write_notes(directory: Path, count: int) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    payloads = [note_bytes(number) for number in range(1, min(count, 2) + 1)]
    # Reuse the two deterministic payloads while keeping unique filenames.
    for number in range(1, count + 1):
        payload = note_bytes(number) if number <= 2 else payloads[(number - 1) % len(payloads)]
        (directory / f"note-{number:05d}.md").write_bytes(payload)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path("target/fixtures/issue23"))
    parser.add_argument("--verify", action="store_true", help="check an existing fixture tree without changing it")
    args = parser.parse_args()
    root = args.root.resolve()
    workspace = Path(__file__).resolve().parent.parent
    base_fixtures = workspace / "target/fixtures"
    large_source = base_fixtures / "markdown_100mb.md"
    image_source = workspace / "assets/phase4-feather.svg"
    if not large_source.is_file():
        raise SystemExit("generate the existing hane-bench fixtures before issue #23 fixtures")
    if not image_source.is_file():
        raise SystemExit(f"missing image fixture source: {image_source}")

    if not args.verify:
        root.mkdir(parents=True, exist_ok=True)
        (root / "small.md").write_bytes(note_bytes(0))
        for name in ("folder_empty", *FOLDER_COUNTS, "large_then_small", "switch_a", "switch_b"):
            path = root / name
            if path.exists():
                shutil.rmtree(path)
        (root / "folder_empty").mkdir()
        for name, count in FOLDER_COUNTS.items():
            write_notes(root / name, count)

        large_then_small = root / "large_then_small"
        large_then_small.mkdir()
        shutil.copyfile(large_source, large_then_small / "00-large.md")
        shutil.copyfile(root / "small.md", large_then_small / "01-small.md")

        for name in ("switch_a", "switch_b"):
            directory = root / name
            directory.mkdir()
            shutil.copyfile(image_source, directory / "image.svg")
            (directory / "00-image.md").write_text(
                "# Image display workload\n\n![Hane feather](image.svg)\n",
                encoding="utf-8",
            )
            (directory / "01-note.md").write_bytes(note_bytes(1))

        manifest = {
            "generator": "scripts/prepare_issue23_fixtures.py",
            "seed": 0,
            "small_note_bytes": NOTE_BYTES,
            "folder_notes": FOLDER_COUNTS,
            "folder_note_bytes": NOTE_BYTES,
            "large_note_source": "target/fixtures/markdown_100mb.md",
            "large_note_bytes": large_source.stat().st_size,
            "switch_folders": 2,
            "image_fixture_source": "assets/phase4-feather.svg",
        }
        (root / "manifest.json").write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )

    small = root / "small.md"
    if not small.is_file():
        raise SystemExit(f"missing small fixture: {small}")
    if small.stat().st_size != NOTE_BYTES:
        raise SystemExit(f"{small}: expected {NOTE_BYTES} bytes, found {small.stat().st_size}")

    for name, count in FOLDER_COUNTS.items():
        folder = root / name
        if not folder.is_dir():
            raise SystemExit(f"missing fixture folder: {folder}")
        notes = sorted(folder.glob("*.md"))
        if len(notes) != count:
            raise SystemExit(f"{name}: expected {count} notes, found {len(notes)}")
        wrong_size = next((path for path in notes if path.stat().st_size != NOTE_BYTES), None)
        if wrong_size is not None:
            raise SystemExit(
                f"{wrong_size}: expected {NOTE_BYTES} bytes, found {wrong_size.stat().st_size}"
            )

    empty_folder = root / "folder_empty"
    if not empty_folder.is_dir():
        raise SystemExit(f"missing fixture folder: {empty_folder}")
    if list(empty_folder.glob("*.md")):
        raise SystemExit(f"expected no Markdown notes in {empty_folder}")

    large_note = root / "large_then_small/00-large.md"
    if not large_note.is_file():
        raise SystemExit(f"missing large note fixture: {large_note}")
    if large_note.stat().st_size != large_source.stat().st_size:
        raise SystemExit(
            f"{large_note}: expected {large_source.stat().st_size} bytes, "
            f"found {large_note.stat().st_size}"
        )
    large_then_small_note = root / "large_then_small/01-small.md"
    if not large_then_small_note.is_file():
        raise SystemExit(f"missing small note fixture: {large_then_small_note}")
    if large_then_small_note.stat().st_size != NOTE_BYTES:
        raise SystemExit(
            f"{large_then_small_note}: expected {NOTE_BYTES} bytes, "
            f"found {large_then_small_note.stat().st_size}"
        )

    for name in ("switch_a", "switch_b"):
        folder = root / name
        if not folder.is_dir():
            raise SystemExit(f"missing fixture folder: {folder}")
        notes = list(folder.glob("*.md"))
        if len(notes) != 2:
            raise SystemExit(f"{name}: expected 2 notes, found {len(notes)}")
        image = folder / "image.svg"
        if not image.is_file():
            raise SystemExit(f"missing image fixture: {image}")
    print(root)


if __name__ == "__main__":
    main()
