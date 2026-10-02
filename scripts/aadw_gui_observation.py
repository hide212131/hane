#!/usr/bin/env python3
"""Report what timed GUI samples can prove; do not infer product causality."""
from __future__ import annotations

import argparse
import json
import math
from pathlib import Path

TIMED = {"lines_coast": 80.0, "pixels_direct_follow": 80.0, "direction_reversal": 55.0}


def number(value) -> bool:
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


def assess_step(step: dict) -> dict:
    name = step.get("name")
    result = {"name": name, "source_result": step.get("result", "unknown"),
              "product_cause_proven": False}
    frames = step.get("frames", [])
    times = []
    if step.get("result") == "blocked" or not isinstance(frames, list) or not frames:
        return {**result, "observation": "unavailable", "failure_class": "measurement",
                "reason": "必要な時間付き画面観測が成立していない。"}
    previous = -1.0
    for frame in frames:
        start = frame.get("capture_started_elapsed_ms")
        end = frame.get("capture_completed_elapsed_ms")
        elapsed = frame.get("elapsed_ms")
        rows = frame.get("visible_lines")
        if (not all(number(value) for value in (start, end, elapsed))
                or not previous <= start <= end or elapsed != end
                or frame.get("event_route") != "cghidEventTap"
                or not isinstance(rows, list) or not rows
                or not all(type(row) is int and 1 <= row <= 500 for row in rows)):
            return {**result, "observation": "unavailable", "failure_class": "measurement",
                    "reason": "時計・入力経路・可視行の記録に欠落または不整合がある。"}
        previous = end
        times.append(end)
    deadline = TIMED[name]
    if name == "direction_reversal":
        gap = step.get("initial_to_reverse_event_ms")
        before = step.get("pre_reverse_capture_completed_after_initial_ms")
        if not number(gap) or not number(before) or not before <= gap <= 135.0:
            return {**result, "observation": "unavailable", "failure_class": "measurement",
                    "reason": "反転前の観測と反転入力が135ms以内に成立していない。"}
    timely = [i for i, value in enumerate(times) if value <= deadline]
    if not timely:
        return {**result, "observation": "unavailable", "failure_class": "measurement",
                "deadline_ms": deadline, "capture_completed_ms": times,
                "reason": "期限内の撮影完了がない。期限後の画像から不合格原因を決めない。"}
    if step.get("result") == "pass":
        return {**result, "observation": "observed_pass", "failure_class": None,
                "deadline_ms": deadline, "timely_frames": timely}
    # A timely unchanged screenshot is an observation, not proof that the app
    # received an input and presented a frame. In particular, screenshot start,
    # completion and WindowServer presentation are not interchangeable clocks.
    return {**result, "observation": "observed_nonpass", "failure_class": "unknown",
            "deadline_ms": deadline, "timely_frames": timely,
            "reason": "期限内の画像はあるが、入力受信・描画・画面取得のどこが原因か未確定。"}


def assess_report(report: dict) -> dict:
    steps = [step for scenario in report.get("scenarios", []) for step in scenario.get("steps", [])
             if step.get("name") in TIMED]
    observed = [assess_step(step) for step in steps]
    missing = sorted(set(TIMED) - {step["name"] for step in observed})
    valid = not missing and len(observed) == len(TIMED) and all(
        item["observation"] == "observed_pass" for item in observed)
    return {"schema_version": 1, "source_procedure": report.get("procedure_version"),
            "diagnosis_required": not valid, "product_fix_supported_by_timing_alone": False,
            "missing_scenarios": missing, "steps": observed}


def annotate(path: Path) -> dict:
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 4 * 1024 * 1024:
        raise ValueError("missing or invalid GUI result")
    report = json.loads(path.read_text(encoding="utf-8"))
    if report.get("verification_kind") != "scroll_inertia_focused":
        return report
    quality = assess_report(report)
    report["observation_quality"] = quality
    if report.get("overall_result") == "pass" and quality["diagnosis_required"]:
        report["overall_result"] = "blocked"
        report["overall_reason"] = "時間計測の成立が未確認のため受け入れ不可。"
        report["summary"] = "[BLOCKED] 時間計測の成立が未確認のため受け入れ不可。"
    path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("result", type=Path)
    args = parser.parse_args()
    report = annotate(args.result)
    # Successful classification does not turn a failed validation into success.
    raise SystemExit(0 if report.get("overall_result") == "pass" else 1)
