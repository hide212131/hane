#!/usr/bin/env python3
"""Report what timed GUI samples can prove; do not infer product causality."""
from __future__ import annotations

import argparse
import json
import math
from pathlib import Path

TIMED = {"lines_coast": 80.0, "pixels_direct_follow": 80.0, "direction_reversal": 55.0}
DISPLAY_TIMED_PROCEDURE = "hosted-scroll-inertia/12"


def number(value) -> bool:
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


def assess_step(step: dict, require_display_response: bool = False) -> dict:
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
        route = step.get("event_route") if name == "direction_reversal" else frame.get("event_route")
        if (not all(number(value) for value in (start, end, elapsed))
                or not previous <= start <= end or elapsed != end
                or route != "cghidEventTap"
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
    if require_display_response and name in {"lines_coast", "pixels_direct_follow"}:
        measurement = step.get("scroll_event_observation")
        if not isinstance(measurement, dict):
            return {**result, "observation": "unavailable", "failure_class": "measurement",
                    "deadline_ms": deadline,
                    "reason": "スクロール入力とWindowServer表示サンプルの計測記録がない。"}
        responses = measurement.get("window_server_display_responses")
        stages = measurement.get("stages_ms")
        if not isinstance(stages, dict):
            stages = {}
        display_stages = stages.get("window_server_display_responses")
        baseline = step.get("baseline")
        if (measurement.get("observation") != "observed_ordered"
                or measurement.get("clock_consistent") is not True
                or measurement.get("window_server_display_observation") != "observed"
                or measurement.get("presentation_observation") != "unavailable"
                or not isinstance(responses, list) or not 1 <= len(responses) <= 64
                or not isinstance(display_stages, list) or len(display_stages) != len(responses)
                or type(baseline) is not int or not 1 <= baseline <= 500):
            return {**result, "observation": "unavailable", "failure_class": "measurement",
                    "deadline_ms": deadline,
                    "reason": "同一ScreenCaptureKitサンプル群の表示時刻・画像・Hane時刻の対応を確認できない。"}
        event_absolute = stages.get("event_post_ms")
        receipt_absolute = stages.get("scroll_receipt_ms")
        paint_absolute = stages.get("frame_paint_ms")
        if (not all(number(value) for value in (event_absolute, receipt_absolute, paint_absolute))
                or not event_absolute <= receipt_absolute <= paint_absolute):
            return {**result, "observation": "unavailable", "failure_class": "measurement",
                    "deadline_ms": deadline,
                    "reason": "Haneのイベント受信・描画時刻が不正。"}
        timely_samples = []
        timely_response = False
        seen_sample_ids = set()
        previous_display_absolute = event_absolute
        previous_callback_absolute = event_absolute
        previous_image_ready_absolute = event_absolute
        previous_artifact_written_absolute = event_absolute
        for index, (response, display_stage) in enumerate(zip(responses, display_stages)):
            if not isinstance(response, dict) or not isinstance(display_stage, dict):
                return {**result, "observation": "unavailable", "failure_class": "measurement",
                        "deadline_ms": deadline,
                        "reason": "WindowServerサンプル記録の構造が不正。"}
            display_elapsed = response.get("display_time_elapsed_ms")
            callback_elapsed = response.get("callback_received_elapsed_ms")
            image_ready_elapsed = response.get("image_ready_elapsed_ms")
            artifact_written_elapsed = response.get("artifact_written_elapsed_ms")
            display_absolute = display_stage.get("window_server_display_ms")
            callback_absolute = display_stage.get("callback_received_ms")
            image_ready_absolute = display_stage.get("image_ready_ms")
            artifact_written_absolute = display_stage.get("artifact_written_ms")
            visible = response.get("visible_lines")
            image_path = response.get("path")
            sample_id = response.get("sample_id")
            if (response.get("frame_status") != "complete"
                    or response.get("timestamp_source") != "SCStreamFrameInfo.displayTime"
                    or response.get("image_source") != "same_CMSampleBuffer"
                    or type(sample_id) is not int or sample_id <= 0
                    or sample_id in seen_sample_ids
                    or type(display_stage.get("sample_id")) is not int
                    or sample_id != display_stage.get("sample_id")
                    or display_stage.get("frame_status") != "complete"
                    or display_stage.get("timestamp_source") != "SCStreamFrameInfo.displayTime"
                    or display_stage.get("image_source") != "same_CMSampleBuffer"
                    or response.get("image_path") != display_stage.get("image_path")
                    or not isinstance(image_path, str)
                    or not image_path.endswith(f"display-response-{index:02d}.png")
                    or response.get("image_path") != image_path
                    or not all(number(value) for value in (
                        display_elapsed, callback_elapsed, image_ready_elapsed, artifact_written_elapsed,
                        display_absolute, callback_absolute, image_ready_absolute,
                        artifact_written_absolute))
                    or callback_elapsed > image_ready_elapsed
                    or image_ready_elapsed > artifact_written_elapsed
                    or display_elapsed > artifact_written_elapsed
                    or not isinstance(visible, list) or not visible
                    or not all(type(row) is int and 1 <= row <= 500 for row in visible)):
                return {**result, "observation": "unavailable", "failure_class": "measurement",
                        "deadline_ms": deadline,
                        "reason": "WindowServer表示・callback・画像保存時刻または同一サンプル画像が不正。"}
            if (not math.isclose(display_absolute - event_absolute, display_elapsed,
                                 rel_tol=0.0, abs_tol=0.001)
                    or not math.isclose(callback_absolute - event_absolute, callback_elapsed,
                                        rel_tol=0.0, abs_tol=0.001)
                    or not math.isclose(image_ready_absolute - event_absolute, image_ready_elapsed,
                                        rel_tol=0.0, abs_tol=0.001)
                    or not math.isclose(artifact_written_absolute - event_absolute,
                                        artifact_written_elapsed, rel_tol=0.0, abs_tol=0.001)
                    or not event_absolute <= callback_absolute <= image_ready_absolute
                    or display_absolute <= previous_display_absolute
                    or callback_absolute < previous_callback_absolute
                    or image_ready_absolute < previous_image_ready_absolute
                    or artifact_written_absolute < previous_artifact_written_absolute
                    or not max(display_absolute, image_ready_absolute) <= artifact_written_absolute):
                return {**result, "observation": "unavailable", "failure_class": "measurement",
                        "deadline_ms": deadline,
                        "reason": "WindowServer表示・callback・artifact時刻の差分または順序が一致しない。"}
            if display_elapsed <= deadline:
                timely_samples.append(display_elapsed)
                changed = min(visible) > baseline
                if changed and display_absolute < paint_absolute:
                    return {**result, "observation": "unavailable", "failure_class": "measurement",
                            "deadline_ms": deadline,
                            "reason": "入力後のスクロール画面がHaneの描画記録より先に表示された。"}
                timely_response = timely_response or changed
            seen_sample_ids.add(sample_id)
            previous_display_absolute = display_absolute
            previous_callback_absolute = callback_absolute
            previous_image_ready_absolute = image_ready_absolute
            previous_artifact_written_absolute = artifact_written_absolute
        if not timely_samples:
            return {**result, "observation": "unavailable", "failure_class": "measurement",
                    "deadline_ms": deadline,
                    "reason": "期限内の完全なWindowServer画面サンプルがなく、80ms応答を測定できない。"}
        if step.get("result") == "pass" and timely_response:
            return {**result, "observation": "observed_pass", "failure_class": None,
                    "deadline_ms": deadline, "window_server_display_elapsed_ms": min(
                        response["display_time_elapsed_ms"] for response in responses
                        if response["display_time_elapsed_ms"] <= deadline
                        and min(response["visible_lines"]) > baseline),
                    "callback_received_elapsed_ms": min(
                        response["callback_received_elapsed_ms"] for response in responses
                        if response["display_time_elapsed_ms"] <= deadline
                        and min(response["visible_lines"]) > baseline),
                    "response_sample_ids": [response["sample_id"] for response in responses
                                            if response["display_time_elapsed_ms"] <= deadline
                                            and min(response["visible_lines"]) > baseline]}
        return {**result, "observation": "observed_nonpass", "failure_class": "unknown",
                "deadline_ms": deadline, "timely_sample_elapsed_ms": timely_samples,
                "reason": ("WindowServer表示時刻の期限内に入力への画面応答を確認できない。"
                           if not timely_response else "測定器の画面判定とproducer判定が一致しない。")}
    timely = [i for i, value in enumerate(times) if value <= deadline]
    if not timely:
        return {**result, "observation": "unavailable", "failure_class": "measurement",
                "deadline_ms": deadline, "capture_completed_ms": times,
                "reason": "期限内の撮影完了がない。期限後の画像から不合格原因を決めない。"}
    if step.get("result") == "pass":
        if name in {"lines_coast", "pixels_direct_follow"}:
            # The producer may use a different response deadline. A timely
            # unchanged image cannot substantiate its later response as ours.
            baseline = step.get("baseline")
            if type(baseline) is not int or not 1 <= baseline <= 500:
                return {**result, "observation": "unavailable", "failure_class": "measurement",
                        "reason": "応答前の可視行が不明なため、期限内の応答を確認できない。"}
            if not any(min(frames[i]["visible_lines"]) > baseline for i in timely):
                return {**result, "observation": "observed_nonpass", "failure_class": "unknown",
                        "deadline_ms": deadline, "timely_frames": timely,
                        "reason": "期限内の画像に応答が見えず、別の応答期限による合格を採用できない。"}
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
    require_display_response = report.get("procedure_version") == DISPLAY_TIMED_PROCEDURE
    observed = [assess_step(step, require_display_response=require_display_response) for step in steps]
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
    # Keep the first judgment even when this function/CLI is used without staging.
    report.setdefault("original_judgment", {
        key: report[key] for key in ("overall_result", "overall_reason", "summary") if key in report
    })
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
