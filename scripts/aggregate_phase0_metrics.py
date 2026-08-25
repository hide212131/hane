#!/usr/bin/env python3
"""Aggregate Hane Phase 0 UI CSV samples into the ADR-0009 Markdown table."""

from __future__ import annotations

import argparse
import csv
import platform
import subprocess
from collections import defaultdict
from pathlib import Path


LATENCY_COLUMNS = {
    "keystroke_to_model": ("input", "keystroke_to_model_ms"),
    "keystroke_to_frame": ("input", "keystroke_to_frame_ms"),
    "frame_interval": ("paint", "frame_interval_ms"),
    "layout": ("paint", "layout_ms"),
    "startup": ("ready", "startup_ms"),
    "file_open": ("ready", "file_open_ms"),
}


def command(*arguments: str) -> str:
    try:
        return subprocess.run(arguments, check=True, capture_output=True, text=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return "unknown"


def percentile(sorted_values: list[float], quantile: float) -> float:
    index = max(0, min(len(sorted_values) - 1, int(len(sorted_values) * quantile + 0.999999) - 1))
    return sorted_values[index]


def distribution(values: list[float]) -> tuple[int, float, float, float, float]:
    values.sort()
    return (
        len(values),
        percentile(values, 0.50),
        percentile(values, 0.95),
        percentile(values, 0.99),
        values[-1],
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--profile", default="release")
    args = parser.parse_args()

    samples: dict[tuple[str, str], list[float]] = defaultdict(list)
    metadata: dict[str, set[str]] = defaultdict(set)
    for path in sorted(args.input.rglob("*.csv")):
        with path.open(newline="", encoding="utf-8") as stream:
            for row in csv.DictReader(stream):
                scenario = row["scenario"]
                metadata["input_source"].add(row["input_source"])
                metadata["refresh_rate_hz"].add(row["refresh_rate_hz"])
                metadata["background_job"].add(row["background_job"])
                for metric, (record_type, column) in LATENCY_COLUMNS.items():
                    if row["record_type"] == record_type and row[column]:
                        value = float(row[column])
                        samples[(scenario, metric)].append(value)
                        if scenario.startswith("100 MB input at ") and metric in {
                            "keystroke_to_model",
                            "keystroke_to_frame",
                            "layout",
                        }:
                            samples[("100 MB input combined", metric)].append(value)
                if row["record_type"] == "input" and row["input_event_kind"] == "ime_commit":
                    if row["keystroke_to_model_ms"]:
                        samples[(scenario, "ime_commit_to_model")].append(float(row["keystroke_to_model_ms"]))
                    if row["keystroke_to_frame_ms"]:
                        samples[(scenario, "ime_commit_to_frame")].append(float(row["keystroke_to_frame_ms"]))
                if row["record_type"] in {"memory_load", "memory_idle_30s"} and row["rss_bytes"]:
                    samples[(scenario, row["record_type"])].append(float(row["rss_bytes"]))
                if scenario.startswith("memory ") and row["record_type"] == "ready" and row["rss_bytes"]:
                    samples[(scenario, "memory_visible_layout")].append(float(row["rss_bytes"]))

    lines = [
        "# Phase 0 UI measurement results",
        "",
        f"- Git: `{command('git', 'rev-parse', 'HEAD')}`",
        f"- Profile: `{args.profile}`",
        f"- Rust: `{command('rustc', '--version')}`",
        "- GPUI: `0.2.2`",
        f"- OS: `{platform.mac_ver()[0] or platform.platform()}`",
        f"- CPU: `{command('sysctl', '-n', 'machdep.cpu.brand_string')}`",
        f"- Input sources: `{', '.join(sorted(metadata['input_source']))}`",
        f"- Refresh rate: `{', '.join(sorted(metadata['refresh_rate_hz']))}` Hz",
        f"- Background job states: `{', '.join(sorted(metadata['background_job']))}`",
        "",
        "| Scenario / metric | Samples | Median | p95 | p99 | Max | Unit |",
        "|---|---:|---:|---:|---:|---:|---|",
    ]
    metric_order = list(LATENCY_COLUMNS) + ["ime_commit_to_model", "ime_commit_to_frame", "memory_load", "memory_visible_layout", "memory_idle_30s"]
    for scenario in sorted({scenario for scenario, _ in samples}):
        for metric in metric_order:
            values = samples.get((scenario, metric), [])
            if not values:
                continue
            count, median, p95, p99, maximum = distribution(values)
            unit = "bytes" if metric.startswith("memory_") else "ms"
            label = metric
            lines.append(
                f"| {scenario} — {label} | {count} | {median:.3f} | {p95:.3f} | {p99:.3f} | {maximum:.3f} | {unit} |"
            )

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(args.output.resolve())


if __name__ == "__main__":
    main()
