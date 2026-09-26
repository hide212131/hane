#!/usr/bin/env python3
"""Aggregate Hane UI CSV samples into the ADR-0009 Markdown table."""

from __future__ import annotations

import argparse
import csv
import platform
import re
import subprocess
import tomllib
from collections import defaultdict
from pathlib import Path


LATENCY_COLUMNS = {
    "keystroke_to_model": ("input", "keystroke_to_model_ms"),
    "keystroke_to_frame": ("input", "keystroke_to_frame_ms"),
    "frame_interval": ("paint", "frame_interval_ms"),
    "layout": ("paint", "layout_ms"),
    "startup": ("ready", "startup_ms"),
    "file_open": ("ready", "file_open_ms"),
    "block_index_update": ("block_index", "block_index_update_ms"),
}

# Counts, not durations: reported with their own unit.
COUNT_COLUMNS = {
    "block_index_reparsed_bytes": ("block_index", "reparsed_bytes", "bytes"),
    "block_index_invalidated_blocks": ("block_index", "invalidated_blocks", "blocks"),
}


def command(*arguments: str) -> str:
    try:
        return subprocess.run(arguments, check=True, capture_output=True, text=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return "unknown"


def gpui_version() -> str:
    manifest = Path(__file__).resolve().parent.parent / "Cargo.toml"
    dependency = tomllib.loads(manifest.read_text(encoding="utf-8"))["workspace"]["dependencies"]["gpui"]
    if isinstance(dependency, str):
        return f"gpui {dependency.lstrip('=')}"
    return f"{dependency.get('package', 'gpui')} {dependency['version'].lstrip('=')}"


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
    parser.add_argument("--label", default="UI")
    args = parser.parse_args()

    samples: dict[tuple[str, str], list[float]] = defaultdict(list)
    metadata: dict[str, set[str]] = defaultdict(set)
    for path in sorted(args.input.rglob("*.csv")):
        with path.open(newline="", encoding="utf-8") as stream:
            for row in csv.DictReader(stream):
                scenario = row.get("scenario")
                if not scenario or not row.get("record_type"):
                    continue
                for key in ("input_source", "refresh_rate_hz", "background_job"):
                    if value := row.get(key):
                        metadata[key].add(value)
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
                for metric, (record_type, column, _) in COUNT_COLUMNS.items():
                    if row["record_type"] == record_type and row.get(column):
                        samples[(scenario, metric)].append(float(row[column]))
                if row["record_type"] == "input" and row["input_event_kind"] == "ime_commit":
                    if row["keystroke_to_model_ms"]:
                        samples[(scenario, "ime_commit_to_model")].append(float(row["keystroke_to_model_ms"]))
                    if row["keystroke_to_frame_ms"]:
                        samples[(scenario, "ime_commit_to_frame")].append(float(row["keystroke_to_frame_ms"]))
                if row["record_type"].startswith("memory_") and row["rss_bytes"]:
                    samples[(scenario, row["record_type"])].append(float(row["rss_bytes"]))
                if scenario.startswith("memory ") and row["record_type"] == "ready" and row["rss_bytes"]:
                    samples[(scenario, "memory_visible_layout")].append(float(row["rss_bytes"]))
                if scenario.startswith("empty ") and row["record_type"] == "ready" and row["rss_bytes"]:
                    samples[(scenario, "memory_ready")].append(float(row["rss_bytes"]))

    work_folder_scans: dict[str, list[tuple[float, int]]] = defaultdict(list)
    scan_pattern = re.compile(
        r"hane_work_folder_ready root=(.*?) elapsed_ms=([0-9.]+) indexed_markdown_files=(\d+)"
    )
    for path in sorted(args.input.rglob("*.log")):
        scenario_dir = path.parent.parent if path.parent.name.startswith("trial_") else path.parent
        scenario = scenario_dir.name
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            if match := scan_pattern.search(line):
                work_folder_scans[scenario].append((float(match.group(2)), int(match.group(3))))

    lines = [
        f"# {args.label} measurement results",
        "",
        f"- Git: `{command('git', 'rev-parse', 'HEAD')}`",
        f"- Profile: `{args.profile}`",
        f"- Rust: `{command('rustc', '--version')}`",
        f"- GPUI: `{gpui_version()}`",
        f"- OS: `{platform.mac_ver()[0] or platform.platform()}`",
        f"- CPU: `{command('sysctl', '-n', 'machdep.cpu.brand_string')}`",
        f"- Input sources: `{', '.join(sorted(metadata['input_source']))}`",
        f"- Refresh rate: `{', '.join(sorted(metadata['refresh_rate_hz']))}` Hz",
        f"- Background job states: `{', '.join(sorted(metadata['background_job']))}`",
        "",
        "| Scenario / metric | Samples | Median | p95 | p99 | Max | Unit |",
        "|---|---:|---:|---:|---:|---:|---|",
    ]
    metric_order = list(LATENCY_COLUMNS) + list(COUNT_COLUMNS) + ["ime_commit_to_model", "ime_commit_to_frame"]
    metric_order.extend(sorted({metric for _, metric in samples if metric.startswith("memory_")}))
    for scenario in sorted({scenario for scenario, _ in samples}):
        for metric in metric_order:
            values = samples.get((scenario, metric), [])
            if not values:
                continue
            count, median, p95, p99, maximum = distribution(values)
            unit = "bytes" if metric.startswith("memory_") else "ms"
            if metric in COUNT_COLUMNS:
                unit = COUNT_COLUMNS[metric][2]
            label = metric
            lines.append(
                f"| {scenario} — {label} | {count} | {median:.3f} | {p95:.3f} | {p99:.3f} | {maximum:.3f} | {unit} |"
            )

    if work_folder_scans:
        lines.extend(
            [
                "",
                "## Work folder scan completion",
                "",
                "| Scenario | Indexed Markdown files | Samples | Median | p95 | Max | Unit |",
                "|---|---:|---:|---:|---:|---:|---|",
            ]
        )
        for scenario, values in sorted(work_folder_scans.items()):
            times = sorted(value[0] for value in values)
            counts = sorted(value[1] for value in values)
            sample_count, median, p95, _, maximum = distribution(times)
            file_count = percentile([float(value) for value in counts], 0.50)
            lines.append(
                f"| {scenario} | {file_count:.0f} | {sample_count} | {median:.3f} | {p95:.3f} | {maximum:.3f} | ms |"
            )

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(args.output.resolve())


if __name__ == "__main__":
    main()
