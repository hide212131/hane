#!/usr/bin/env python3
"""Focused hosted GUI validation for Issue #389's wheel-scroll behavior."""

from __future__ import annotations

import importlib.util
import hashlib
import json
import math
import os
import platform
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Optional

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "hosted-scroll-inertia/19"
VERIFICATION_KIND = "scroll_inertia_focused"
SCOPE_NOTE = (
    "Issue #389 に限定した focused GUI evidence。Lines の初回応答・解放後の余韻と減速・"
    "逆方向入力への切替、文書先頭/末尾のクランプ、Pixels の直接追従と安定を実画面で確認する。"
    "入力イベントは ScrollDelta 相当の Lines / Pixels を明示して発生させ、端末種別は推測しない。"
    "Lines / Pixelsの80ms初回応答は、cghidEventTap後の実画面画像が80ms以内に取得完了した場合だけ確認する。"
    "wheel-captureとwheel-measureを同じ文書先頭・同じ入力値で各2回比較する。wheel-captureは実際の画像取得経路を維持したままHane側の受信・描画時刻も記録する。"
    "余韻・減速・安定は従来の画面取得系列で135msの慣性窓内を確認し、callback遅延と表示時刻を分けて記録する。"
    "中間位置への位置決めは較正済みの通常画面取得経路で送り、複数時点の画面で位置のみ確認する。受入判定には使わない。"
    "Pixelsは応答付近を連続して撮影し、反転入力は複数の旧方向候補画面を撮影した直後に送り、OCRはその後に行って慣性窓を消費しない。"
    "文書先頭のOCR前に挿入カーソルを先頭行末へ移し、先頭文字の読み取りを妨げない。"
    "両入力は共通のcghidEventTap経路で送り、経路と画面応答を記録する。"
)
EXIT_PASS = 0
EXIT_NONPASS = 1
LINE_COUNT = 500
# 108ms -> 144ms (Lines) and 112ms -> 160ms (Pixels) both jump past the 135ms
# deadline. Capture completion trails its scheduled start, so a response that
# only appears around 120ms could otherwise have no remaining capture point
# able to finish inside the window. 120ms/128ms close that gap while keeping
# every previously existing delay unchanged.
FRAME_DELAYS_MS = (0, 24, 40, 64, 108, 120, 144, 190, 240)
REVERSE_FRAME_DELAYS_MS = (0, 24, 48, 80, 120, 180)
PIXELS_FRAME_DELAYS_MS = (0, 24, 40, 64, 88, 112, 128, 160, 200)
# Run 37082138091 showed that a 120ms probe can finish after the existing
# 135ms Lines window on the hosted macOS runner. The current window displayed
# old-direction motion by 67ms, so keep three probes but start the last one at
# 96ms, leaving 39ms for screen capture and scheduler delay. This changes only
# when the observer samples; the 135ms acceptance window is unchanged.
PRE_REVERSE_PROBE_DELAYS_MS = (32, 72)
LINES_INERTIA_WINDOW_MS = 135.0
WHEEL_MEASURE_ERROR_OUTPUT_LIMIT = 4000
# Match the direction-calibration probe that moved the document on hosted macOS.
# A single 120-line positioning event failed to move the same fixture.
POSITIONING_LINES_DELTA = 32
POSITIONING_MAX_STEPS = 16
POSITIONING_FRAME_DELAYS_MS = (0, 48, 96, 144, 200, 240)
MIDPOINT_BAND_START = LINE_COUNT // 3
MIDPOINT_BAND_END = LINE_COUNT * 2 // 3
# The product trajectory evaluator uses the Issue's 135ms coast window.
# Procedure /15 separately verifies the 80ms first visual response using the
# WindowServer display timestamp attached to the same streamed image sample.
VISIBLE_RESPONSE_WINDOW_MS = LINES_INERTIA_WINDOW_MS
FIRST_RESPONSE_LIMIT_MS = 80.0


def load_module(control_dir: Path, relative: str, name: str):
    sys.dont_write_bytecode = True
    module_path = control_dir / relative
    spec = importlib.util.spec_from_file_location(name, module_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load trusted module: {module_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def step(name: str, result: str, reason: Optional[str] = None, **detail) -> dict:
    return {"name": name, "result": result, "reason": reason, **detail}


def bounded_wheel_measure_output(output: str) -> str:
    excerpt = output[:WHEEL_MEASURE_ERROR_OUTPUT_LIMIT]
    return excerpt if len(output) <= WHEEL_MEASURE_ERROR_OUTPUT_LIMIT else excerpt + "\n...(truncated)"


def scroll_event_timing_log_delta(path: Path, offset: int) -> dict:
    """Preserve the exact bounded product timing-log bytes appended by one
    wheel-measure invocation, including an explicit record when no bytes were
    available. This is diagnostic evidence only and never changes acceptance."""
    try:
        content = path.read_bytes()
    except FileNotFoundError:
        return {"status": "missing", "appended_bytes": 0, "content_excerpt": "",
                "truncated": False}
    except OSError as exc:
        return {"status": "read_error", "appended_bytes": 0,
                "content_excerpt": str(exc)[:WHEEL_MEASURE_ERROR_OUTPUT_LIMIT],
                "truncated": False}

    file_was_truncated = len(content) < offset
    appended = content if file_was_truncated else content[offset:]
    excerpt = appended[:WHEEL_MEASURE_ERROR_OUTPUT_LIMIT]
    return {
        "status": "truncated_before_read" if file_was_truncated else (
            "appended" if appended else "no_new_bytes"),
        "appended_bytes": len(appended),
        "content_excerpt": excerpt.decode("utf-8", errors="replace"),
        "truncated": len(appended) > len(excerpt),
    }


def skipped(name: str, reason: str) -> dict:
    return step(name, "skipped", reason)


def attach_scroll_event_observation(step_result: dict, observation: Optional[dict]) -> dict:
    """Adds Issue #427's separate mach-clock observer classification to an
    already-judged step without ever reading or changing that step's own
    `result`/`reason`, so a disordered or unavailable observation can never
    upgrade a fail/blocked producer judgment into a pass, and an ordered
    observation can never downgrade or hide one."""
    return {**step_result, "scroll_event_observation": observation}


def visible_lines(text: str) -> list[int]:
    return sorted({int(value) for value in re.findall(r"\bLINE\s+(\d+)\b", text, re.I)})


def first_visible(frame: dict) -> Optional[int]:
    lines = frame.get("visible_lines", [])
    return min(lines) if lines else None


def last_visible(frame: dict) -> Optional[int]:
    lines = frame.get("visible_lines", [])
    return max(lines) if lines else None


def midpoint_band_is_visible(lines: list[int]) -> bool:
    """Require at least one OCR-recognized line from the fixture's middle third."""
    return any(MIDPOINT_BAND_START <= line <= MIDPOINT_BAND_END for line in lines)


def evaluate_first_response(baseline: Optional[int], frames: list[dict], name: str,
                            response_limit_ms: float = FIRST_RESPONSE_LIMIT_MS) -> dict:
    """Require an actual captured screen image to show movement by the response limit."""
    offsets = [first_visible(frame) for frame in frames]
    times = [frame.get("elapsed_ms") for frame in frames]
    if baseline is None or not offsets or any(value is None for value in offsets):
        return step(name, "blocked", "初回応答を判定する可視行番号が足りない",
                    baseline=baseline, offsets=offsets, elapsed_ms=times,
                    response_limit_ms=response_limit_ms)
    response_index = next((index for index, (offset, elapsed) in enumerate(zip(offsets, times))
                           if offset > baseline and isinstance(elapsed, (int, float))), None)
    if response_index is None:
        return step(name, "fail", "取得した画面にスクロール応答が現れない",
                    baseline=baseline, offsets=offsets, elapsed_ms=times,
                    response_limit_ms=response_limit_ms)
    response_ms = times[response_index]
    passed = 0 <= response_ms <= response_limit_ms
    return step(name, "pass" if passed else "fail",
                None if passed else f"画面応答の取得完了が{response_limit_ms:g}msを超えた",
                baseline=baseline, offsets=offsets, elapsed_ms=times,
                first_response_frame=response_index, first_response_ms=response_ms,
                response_limit_ms=response_limit_ms)


def classify_measurement_path_comparison(capture_response: dict, measure_response: dict,
                                         same_start_lines: bool) -> dict:
    """Describe the two observers without changing product acceptance."""
    capture_result = capture_response.get("result")
    measure_result = measure_response.get("result")
    complete = (same_start_lines and capture_result in {"pass", "fail"}
                and measure_result in {"pass", "fail"})
    if not complete:
        classification = "unknown"
    elif capture_result != measure_result:
        classification = "measurement_path_divergence"
    else:
        classification = "same_observed_outcome"
    return {
        "result": "pass" if complete else "blocked",
        "classification": classification,
        "same_start_lines": same_start_lines,
        "wheel_capture_response": capture_response,
        "wheel_measure_response": measure_response,
    }


def aggregate_acceptance_result(steps: list[dict], priority: dict[str, int]) -> tuple[str, list[str]]:
    """Aggregate product acceptance steps without treating a diagnostic as acceptance."""
    acceptance_steps = [item for item in steps
                        if item.get("name") != "wheel_measurement_path_comparison"]
    considered = [item["result"] for item in acceptance_steps
                  if item.get("result") in priority]
    result = min(considered, key=lambda value: priority[value]) if considered else "blocked"
    reasons = [item.get("reason") for item in acceptance_steps
               if item.get("result") not in ("pass", "skipped") and item.get("reason")]
    return result, reasons


def evaluate_lines_coast(baseline: Optional[int], frames: list[dict]) -> dict:
    offsets = [first_visible(frame) for frame in frames]
    times = [frame.get("elapsed_ms") for frame in frames]
    valid = baseline is not None and len(offsets) >= 6 and all(value is not None for value in offsets)
    if not valid:
        return step("lines_coast", "blocked", "Lines 時系列の可視行番号を十分に読み取れない",
                    baseline=baseline, offsets=offsets, elapsed_ms=times, frames=frames)
    first_response_index = next(
        (index for index, (offset, elapsed) in enumerate(zip(offsets, times))
         if offset > baseline and 0 <= elapsed <= VISIBLE_RESPONSE_WINDOW_MS),
        None,
    )
    first_response = first_response_index is not None
    last_index = len(offsets) - 1
    split_index = 4
    if first_response_index is not None and first_response_index >= split_index:
        # The fixed split frame is at or after the response here, so
        # comparing against it would mix pre-response frames into the
        # "early" rate (or compare the response frame against itself).
        # Split the remaining post-response frames instead; if too few
        # remain to compare early vs. late speed, this is insufficient
        # observation rather than a product failure.
        remaining = last_index - first_response_index
        if remaining < 2:
            return step(
                "lines_coast", "blocked",
                "初回応答後に減速を判定できる十分な時系列区間がない",
                baseline=baseline, offsets=offsets, elapsed_ms=times, first_response=first_response,
                first_response_frame=first_response_index, frames=frames,
            )
        split_index = first_response_index + remaining // 2
    continued = any(after > before for before, after in zip(offsets, offsets[1:]))
    monotonic = all(after >= before - 1 for before, after in zip(offsets, offsets[1:]))
    early_start = first_response_index if first_response_index is not None else 0
    early_elapsed = max(1.0, times[split_index] - times[early_start])
    late_elapsed = max(1.0, times[-1] - times[split_index])
    early_rate = max(0, offsets[split_index] - offsets[early_start]) / early_elapsed
    late_rate = max(0, offsets[-1] - offsets[split_index]) / late_elapsed
    decelerated = early_rate > 0 and late_rate < early_rate
    settled = abs(offsets[-1] - offsets[-2]) <= 1 and times[-1] >= 180
    passed = first_response and continued and monotonic and decelerated and settled
    reasons = []
    if not first_response:
        reasons.append("Linesの慣性窓内（135ms以内）の画面観測で入力への応答を確認できない")
    if not continued:
        reasons.append("入力解放後の余韻による追加移動を確認できない")
    if not monotonic:
        reasons.append("通常スクロール中に旧方向と反対への段差を観測した")
    if not decelerated:
        reasons.append("後半の移動速度が前半より下がることを確認できない")
    if not settled:
        reasons.append("200ms付近で移動が収束したことを確認できない")
    return step(
        "lines_coast", "pass" if passed else "fail", None if passed else "。".join(reasons),
        baseline=baseline, offsets=offsets, elapsed_ms=times, first_response=first_response,
        first_response_frame=first_response_index,
        continued_after_release=continued, monotonic=monotonic, early_lines_per_ms=early_rate,
        late_lines_per_ms=late_rate, decelerated=decelerated, settled=settled, frames=frames,
    )


def evaluate_reversal(baseline: Optional[int], pre_reverse: Optional[int], frames: list[dict],
                      initial_to_reverse_event_ms: Optional[float],
                      event_route: Optional[str] = None,
                      pre_reverse_capture_completed_after_initial_ms: Optional[float] = None) -> dict:
    offsets = [first_visible(frame) for frame in frames]
    times = [frame.get("elapsed_ms") for frame in frames]
    valid = (baseline is not None and pre_reverse is not None
             and initial_to_reverse_event_ms is not None
             and len(offsets) >= 4 and all(value is not None for value in offsets))
    if not valid:
        return step("direction_reversal", "blocked", "反転前後の可視行番号を十分に読み取れない",
                    baseline=baseline, pre_reverse=pre_reverse, offsets=offsets,
                    initial_to_reverse_event_ms=initial_to_reverse_event_ms,
                    event_route=event_route, elapsed_ms=times, frames=frames)
    if initial_to_reverse_event_ms > LINES_INERTIA_WINDOW_MS:
        return step(
            "direction_reversal", "blocked",
            "初回Linesイベントから反対方向イベントまでが慣性の持続時間を超え、方向反転を判定できない",
            baseline=baseline, pre_reverse=pre_reverse, offsets=offsets,
            initial_to_reverse_event_ms=initial_to_reverse_event_ms,
            inertia_window_ms=LINES_INERTIA_WINDOW_MS, event_route=event_route,
            elapsed_ms=times, frames=frames,
        )
    if (pre_reverse_capture_completed_after_initial_ms is None
            or not 0 <= pre_reverse_capture_completed_after_initial_ms
            <= initial_to_reverse_event_ms <= LINES_INERTIA_WINDOW_MS):
        return step(
            "direction_reversal", "blocked",
            "旧方向への動きを含む画面の撮影完了が慣性窓内か確認できない",
            baseline=baseline, pre_reverse=pre_reverse, offsets=offsets,
            initial_to_reverse_event_ms=initial_to_reverse_event_ms,
            pre_reverse_capture_completed_after_initial_ms=pre_reverse_capture_completed_after_initial_ms,
            inertia_window_ms=LINES_INERTIA_WINDOW_MS, event_route=event_route,
            elapsed_ms=times, frames=frames,
        )
    old_direction_started = pre_reverse > baseline
    prompt = offsets[0] <= pre_reverse + 1 and 0 <= times[0] <= 55
    reversed_direction = offsets[-1] < pre_reverse
    no_old_coast = all(value <= pre_reverse + 1 for value in offsets)
    passed = old_direction_started and prompt and reversed_direction and no_old_coast
    reasons = []
    if not old_direction_started:
        reasons.append("反転前にLines入力で旧方向へ動いたことを確認できない")
    if not prompt:
        reasons.append("反対方向入力の直後も古い方向へ移動した")
    if not reversed_direction:
        reasons.append("反対方向への移動を確認できない")
    if not no_old_coast:
        reasons.append("反転後に古い方向への余韻が残った")
    return step(
        "direction_reversal", "pass" if passed else "fail", None if passed else "。".join(reasons),
        baseline=baseline, pre_reverse=pre_reverse, offsets=offsets,
        initial_to_reverse_event_ms=initial_to_reverse_event_ms,
        pre_reverse_capture_completed_after_initial_ms=pre_reverse_capture_completed_after_initial_ms,
        inertia_window_ms=LINES_INERTIA_WINDOW_MS, elapsed_ms=times,
        event_route=event_route, old_direction_started=old_direction_started, prompt=prompt,
        reversed_direction=reversed_direction, no_old_coast=no_old_coast, frames=frames,
    )


def evaluate_pixels(baseline: Optional[int], frames: list[dict]) -> dict:
    offsets = [first_visible(frame) for frame in frames]
    times = [frame.get("elapsed_ms") for frame in frames]
    valid = baseline is not None and len(offsets) >= 3 and all(value is not None for value in offsets)
    if not valid:
        return step("pixels_direct_follow", "blocked", "Pixels 時系列の可視行番号を十分に読み取れない",
                    baseline=baseline, offsets=offsets, elapsed_ms=times, frames=frames)
    first_response_index = next(
        (index for index, (offset, elapsed) in enumerate(zip(offsets, times))
         if offset > baseline and 0 <= elapsed <= VISIBLE_RESPONSE_WINDOW_MS),
        None,
    )
    immediate = first_response_index is not None
    stable = (first_response_index is not None
              and abs(offsets[-1] - offsets[first_response_index]) <= 1
              and times[-1] >= 160)
    passed = immediate and stable
    reasons = []
    if not immediate:
        reasons.append("135ms以内の画面観測でPixels入力への直接追従を確認できない")
    if not stable:
        reasons.append("入力後にアプリ側の追加慣性がないことを確認できない")
    return step(
        "pixels_direct_follow", "pass" if passed else "fail", None if passed else "。".join(reasons),
        baseline=baseline, offsets=offsets, elapsed_ms=times, immediate=immediate,
        first_response_frame=first_response_index,
        stable_without_app_coast=stable, frames=frames,
    )


def evaluate_document_edge(frames: list[dict], edge: str) -> dict:
    if edge not in {"top", "bottom"}:
        raise ValueError(f"unsupported document edge: {edge}")
    marker = 1 if edge == "top" else LINE_COUNT
    visible = [frame.get("visible_lines", []) for frame in frames]
    times = [frame.get("elapsed_ms") for frame in frames]
    valid = len(visible) >= 4 and all(rows for rows in visible)
    if not valid:
        return step(f"document_{edge}_edge", "blocked", "端の画面時系列を十分に読み取れない",
                    marker=marker, visible_lines=visible, elapsed_ms=times, frames=frames)
    reached = [marker in rows for rows in visible]
    first_reached = next((index for index, value in enumerate(reached) if value), None)
    stayed = first_reached is not None and all(reached[first_reached:])
    stable = len(reached) >= 2 and reached[-1] and reached[-2] and times[-1] >= 180
    passed = stayed and stable
    reasons = []
    if first_reached is None:
        reasons.append("文書端の行が画面に現れない")
    elif not stayed:
        reasons.append("端の行が現れた後にスクロールで見えなくなった")
    if not stable:
        reasons.append("最後の画面観測で端に収束したことを確認できない")
    return step(
        f"document_{edge}_edge", "pass" if passed else "fail",
        None if passed else "。".join(reasons), marker=marker,
        visible_lines=visible, elapsed_ms=times, first_reached_frame=first_reached,
        stayed_at_edge=stayed, stable=stable, frames=frames,
    )


def env_float(name: str, default: float) -> float:
    value = os.environ.get(name)
    if not value:
        return default
    parsed = float(value)
    if parsed != parsed or parsed <= 0 or parsed == float("inf"):
        raise ValueError(f"{name} must be positive and finite")
    return parsed


def capture_ocr(interaction, helper, path: Path, helper_timeout: float) -> tuple[Optional[list[int]], str, Optional[str]]:
    ok, text, error = interaction.run_helper(helper, ["ocr", str(path)], helper_timeout)
    return (visible_lines(text), text, None) if ok else (None, "", error)


def parse_reversal_helper_output(output: str, expected_pre_frames: int, expected_frames: int) -> dict:
    fields = {}
    route = None
    for line in output.splitlines():
        name, separator, value = line.partition("=")
        if separator:
            if name == "reversal_event_route":
                route = value
                continue
            try:
                parsed = float(value)
            except ValueError:
                continue
            if math.isfinite(parsed):
                fields[name] = parsed
    required = {"initial_event_elapsed_ms", "reverse_event_elapsed_ms"}
    for index in range(expected_pre_frames):
        required.add(f"pre_frame_{index:02d}_capture_started_ms")
        required.add(f"pre_frame_{index:02d}_capture_completed_ms")
    for index in range(expected_frames):
        required.add(f"frame_{index:02d}_capture_started_ms")
        required.add(f"frame_{index:02d}_capture_completed_ms")
    missing = sorted(required - fields.keys())
    if missing:
        raise ValueError(f"reversal helper timing is missing: {', '.join(missing)}")
    if route != "cghidEventTap":
        raise ValueError("reversal helper did not use the cghidEventTap route")
    if not fields["initial_event_elapsed_ms"] <= fields["reverse_event_elapsed_ms"]:
        raise ValueError("reversal helper event times are out of order")
    pre_started = [fields[f"pre_frame_{index:02d}_capture_started_ms"] for index in range(expected_pre_frames)]
    pre_completed = [fields[f"pre_frame_{index:02d}_capture_completed_ms"] for index in range(expected_pre_frames)]
    if (any(start < fields["initial_event_elapsed_ms"] or completed < start
            for start, completed in zip(pre_started, pre_completed))
            or pre_started != sorted(pre_started)
            or pre_completed != sorted(pre_completed)
            or any(completed > fields["reverse_event_elapsed_ms"] for completed in pre_completed)):
        raise ValueError("reversal helper pre-reversal capture times are out of order")
    frame_started = [fields[f"frame_{index:02d}_capture_started_ms"] for index in range(expected_frames)]
    frame_completed = [fields[f"frame_{index:02d}_capture_completed_ms"] for index in range(expected_frames)]
    if (any(start < 0 or completed < start for start, completed in zip(frame_started, frame_completed))
            or frame_started != sorted(frame_started)
            or frame_completed != sorted(frame_completed)):
        raise ValueError("reversal helper post-event frame times are invalid")
    return {
        "event_route": route,
        "initial_to_reverse_event_ms": (
            fields["reverse_event_elapsed_ms"] - fields["initial_event_elapsed_ms"]
        ),
        "pre_frame_capture_started_after_initial_ms": [
            value - fields["initial_event_elapsed_ms"] for value in pre_started
        ],
        "pre_frame_capture_completed_after_initial_ms": [
            value - fields["initial_event_elapsed_ms"] for value in pre_completed
        ],
        "frame_elapsed_ms": frame_completed,
        "frame_capture_started_ms": frame_started,
        "frame_capture_completed_ms": frame_completed,
    }


def parse_scroll_capture_helper_output(output: str, expected_frames: int) -> dict:
    fields = {}
    route = None
    for line in output.splitlines():
        name, separator, value = line.partition("=")
        if not separator:
            continue
        if name == "event_route":
            route = value
            continue
        try:
            parsed = float(value)
        except ValueError:
            continue
        if math.isfinite(parsed):
            fields[name] = parsed
    required = {"event_post_elapsed_ms"}
    for index in range(expected_frames):
        required.add(f"frame_{index:02d}_capture_started_ms")
        required.add(f"frame_{index:02d}_capture_completed_ms")
    missing = sorted(required - fields.keys())
    if missing:
        raise ValueError(f"scroll helper timing is missing: {', '.join(missing)}")
    if route != "cghidEventTap":
        raise ValueError("scroll helper did not use the cghidEventTap route")
    frame_started = [fields[f"frame_{index:02d}_capture_started_ms"] for index in range(expected_frames)]
    frame_completed = [fields[f"frame_{index:02d}_capture_completed_ms"] for index in range(expected_frames)]
    if (any(start < 0 or completed < start for start, completed in zip(frame_started, frame_completed))
            or frame_started != sorted(frame_started)
            or frame_completed != sorted(frame_completed)):
        raise ValueError("scroll helper frame times are invalid")
    return {
        "event_route": route,
        "event_post_elapsed_ms": fields["event_post_elapsed_ms"],
        "frame_elapsed_ms": frame_completed,
        "frame_capture_started_ms": frame_started,
        "frame_capture_completed_ms": frame_completed,
    }


def parse_wheel_measure_capture_output(output: str, expected_frames: int) -> dict:
    """Converts `wheel-measure`'s raw mach-tick fields (Issue #427) into the
    same elapsed-ms-since-event-post shape `parse_scroll_capture_helper_output`
    already produces, so swapping the capture command for the measurement
    build changes only how timing is extracted, never `evaluate_lines_coast`/
    `evaluate_pixels`'s judgment itself. WindowServer response validation is
    returned separately so a partial response collection cannot discard
    already-captured positioning frames."""
    fields: dict[str, str] = {}
    route = None
    for line in output.splitlines():
        name, separator, value = line.partition("=")
        if not separator:
            continue
        if name == "event_route":
            route = value
            continue
        fields[name] = value
    if route != "cghidEventTap":
        raise ValueError("scroll helper did not use the cghidEventTap route")

    def uint(name: str) -> Optional[int]:
        value = fields.get(name)
        if value is None or value == "unavailable":
            return None
        try:
            parsed = int(value)
        except ValueError:
            return None
        if parsed < 0 or parsed > 0xFFFFFFFFFFFFFFFF:
            return None
        return parsed

    event_post_ticks = uint("event_post_ticks")
    numer = uint("mach_timebase_numer")
    denom = uint("mach_timebase_denom")
    if (event_post_ticks is None or not numer or not denom
            or numer > 0xFFFFFFFF or denom > 0xFFFFFFFF):
        raise ValueError("scroll helper timing is missing: event_post_ticks/mach_timebase_numer/mach_timebase_denom")

    def ticks_to_elapsed_ms(ticks: int) -> float:
        try:
            return (ticks - event_post_ticks) * numer / denom / 1_000_000.0
        except OverflowError:
            return float("inf")

    frame_started = []
    frame_completed = []
    for index in range(expected_frames):
        started = uint(f"frame_{index:02d}_capture_started_ticks")
        completed = uint(f"frame_{index:02d}_capture_completed_ticks")
        if started is None or completed is None:
            raise ValueError(f"scroll helper timing is missing: frame_{index:02d}_capture_started/completed_ticks")
        frame_started.append(ticks_to_elapsed_ms(started))
        frame_completed.append(ticks_to_elapsed_ms(completed))
    if (any(start < 0 or completed < start for start, completed in zip(frame_started, frame_completed))
            or frame_started != sorted(frame_started)
            or frame_completed != sorted(frame_completed)):
        raise ValueError("scroll helper frame times are invalid")

    def parse_display_responses() -> list[dict]:
        response_count = uint("display_response_count")
        if (fields.get("display_response_collection_valid") != "true"
                or response_count is None or not 1 <= response_count <= 64):
            raise ValueError("WindowServer display response collection is missing, invalid, or incomplete")
        responses = []
        seen_sample_ids = set()
        previous_display_ticks = event_post_ticks - 1
        previous_callback_ticks = event_post_ticks
        previous_image_ready_ticks = event_post_ticks
        previous_artifact_written_ticks = event_post_ticks
        for index in range(response_count):
            prefix = f"display_response_{index:02d}_"
            sample_id = uint(prefix + "sample_id")
            display_ticks = uint(prefix + "display_time_ticks")
            callback_ticks = uint(prefix + "callback_received_ticks")
            image_ready_ticks = uint(prefix + "image_ready_ticks")
            artifact_written_ticks = uint(prefix + "artifact_written_ticks")
            if (fields.get(prefix + "frame_status") != "complete"
                    or fields.get(prefix + "timestamp_source") != "SCStreamFrameInfo.displayTime"
                    or fields.get(prefix + "image_source") != "same_CMSampleBuffer"
                    or sample_id is None or sample_id == 0 or sample_id in seen_sample_ids
                    or display_ticks is None or display_ticks == 0
                    or callback_ticks is None or image_ready_ticks is None
                    or artifact_written_ticks is None
                    or not (fields.get(prefix + "image_path") or "").endswith(
                        f"display-response-{index:02d}.png")):
                raise ValueError(f"WindowServer display response {index} is missing valid same-sample metadata")
            if (display_ticks < event_post_ticks or callback_ticks < event_post_ticks
                    or display_ticks <= previous_display_ticks
                    or callback_ticks < previous_callback_ticks
                    or image_ready_ticks < max(callback_ticks, previous_image_ready_ticks)
                    or artifact_written_ticks < previous_artifact_written_ticks
                    or artifact_written_ticks < max(image_ready_ticks, display_ticks)):
                raise ValueError(f"WindowServer display response {index} timestamps are out of order")
            display_elapsed_ms = ticks_to_elapsed_ms(display_ticks)
            callback_elapsed_ms = ticks_to_elapsed_ms(callback_ticks)
            image_ready_elapsed_ms = ticks_to_elapsed_ms(image_ready_ticks)
            artifact_written_elapsed_ms = ticks_to_elapsed_ms(artifact_written_ticks)
            if (not all(math.isfinite(value) for value in (
                    display_elapsed_ms, callback_elapsed_ms, image_ready_elapsed_ms, artifact_written_elapsed_ms))
                    or display_elapsed_ms > LINES_INERTIA_WINDOW_MS):
                raise ValueError(f"WindowServer display response {index} is late or not finite")
            responses.append({
                "sample_id": sample_id,
                "frame_status": fields[prefix + "frame_status"],
                "timestamp_source": fields[prefix + "timestamp_source"],
                "image_source": fields[prefix + "image_source"],
                "display_time_ticks": display_ticks,
                "callback_received_ticks": callback_ticks,
                "image_ready_ticks": image_ready_ticks,
                "artifact_written_ticks": artifact_written_ticks,
                "image_path": fields.get(prefix + "image_path"),
                "display_time_elapsed_ms": display_elapsed_ms,
                "callback_received_elapsed_ms": callback_elapsed_ms,
                "image_ready_elapsed_ms": image_ready_elapsed_ms,
                "artifact_written_elapsed_ms": artifact_written_elapsed_ms,
            })
            seen_sample_ids.add(sample_id)
            previous_display_ticks = display_ticks
            previous_callback_ticks = callback_ticks
            previous_image_ready_ticks = image_ready_ticks
            previous_artifact_written_ticks = artifact_written_ticks
        return responses

    try:
        display_responses = parse_display_responses()
        display_response_error = None
    except ValueError as exc:
        display_responses = []
        display_response_error = str(exc)
    return {
        "event_route": route,
        "frame_elapsed_ms": frame_completed,
        "frame_capture_started_ms": frame_started,
        "frame_capture_completed_ms": frame_completed,
        "display_responses": display_responses,
        "display_response_error": display_response_error,
    }


def select_old_direction_candidate(baseline: int, candidates: list[dict]) -> Optional[dict]:
    for candidate in reversed(candidates):
        lines = candidate.get("visible_lines")
        if not lines:
            continue
        value = min(lines)
        if value > baseline:
            return {**candidate, "value": value}
    return None


def capture_frames(interaction, module, env, config, helper, pid: int, window_id: str,
                   run_dir: Path, unit: str, delta: int, delays: tuple[int, ...],
                   helper_timeout: float, reverse_delta: Optional[int] = None,
                   baseline: Optional[int] = None,
                   pre_reverse_probe_delays_ms: tuple[int, ...] = PRE_REVERSE_PROBE_DELAYS_MS,
                   scroll_event_timing_path: Optional[Path] = None,
                   scroll_event_observation_module: Optional[object] = None,
                   capture_scroll_event_timing: bool = False,
                   scroll_event_poll_timeout_ms: int = 1500,
                   ) -> tuple[list[dict], Optional[dict], Optional[str]]:
    """Optionally correlates the existing screenshot capture with Hane's
    event-receipt and frame-paint ticks. `capture_scroll_event_timing` keeps
    the normal `wheel-capture` input/screenshot path and adds only timing
    fields; the measurement-only `wheel-measure` path remains separately
    selectable. Neither observation changes product acceptance."""
    if reverse_delta is not None and baseline is None:
        return [], None, "反転前の基準可視行を読み取れず、方向反転を実行できない"
    run_dir.mkdir(parents=True, exist_ok=True)
    if reverse_delta is not None:
        pre_frame_dir = run_dir / "pre-reversal-frames"
        pre_frame_dir.mkdir(parents=True, exist_ok=True)
        frame_dir = run_dir / "reverse-frames"
        frame_dir.mkdir(parents=True, exist_ok=True)
        ok, output, error = interaction.run_helper(helper, [
            "wheel-reversal", str(pid), unit, str(delta), str(reverse_delta),
            ",".join(str(delay) for delay in pre_reverse_probe_delays_ms),
            str(window_id), str(pre_frame_dir), str(frame_dir),
            ",".join(str(delay) for delay in delays),
        ], helper_timeout)
        if not ok:
            return [], None, error
        try:
            evidence = parse_reversal_helper_output(
                output, len(pre_reverse_probe_delays_ms), len(delays))
        except ValueError as exc:
            return [], None, str(exc)

        candidates = []
        for index, completed in enumerate(evidence["pre_frame_capture_completed_after_initial_ms"]):
            path = pre_frame_dir / f"pre-frame-{index:02d}.png"
            lines, text, ocr_error = capture_ocr(interaction, helper, path, helper_timeout)
            if ocr_error:
                return [], None, ocr_error
            candidates.append({
                "path": str(path),
                "capture_started_after_initial_ms": evidence["pre_frame_capture_started_after_initial_ms"][index],
                "capture_completed_after_initial_ms": completed,
                "visible_lines": lines, "recognized_text": text,
            })
        pre_reverse = {
            "event_route": evidence["event_route"],
            "initial_to_reverse_event_ms": evidence["initial_to_reverse_event_ms"],
            "candidates": candidates,
            "selected": select_old_direction_candidate(baseline, candidates),
        }

        frames = []
        for index, elapsed in enumerate(evidence["frame_elapsed_ms"]):
            path = frame_dir / f"frame-{index:02d}.png"
            lines, text, ocr_error = capture_ocr(interaction, helper, path, helper_timeout)
            if ocr_error:
                return [], pre_reverse, ocr_error
            frames.append({"path": str(path),
                           "capture_started_elapsed_ms": evidence["frame_capture_started_ms"][index],
                           "elapsed_ms": elapsed,
                           "capture_completed_elapsed_ms": evidence["frame_capture_completed_ms"][index],
                           "visible_lines": lines, "recognized_text": text})
        return frames, pre_reverse, None

    frame_dir = run_dir / "frames"
    frame_dir.mkdir(parents=True, exist_ok=True)
    scroll_event_observation = None
    use_wheel_measure = (
        not capture_scroll_event_timing
        and scroll_event_timing_path is not None
        and scroll_event_observation_module is not None
    )
    use_timed_wheel_capture = capture_scroll_event_timing
    if use_timed_wheel_capture and (scroll_event_timing_path is None
                                    or scroll_event_observation_module is None):
        return [], None, "Hane側スクロール時刻を取得するcapture pathの設定が不足している"
    timing_log_offset = 0
    if scroll_event_timing_path is not None:
        try:
            timing_log_offset = scroll_event_timing_path.stat().st_size
        except (FileNotFoundError, OSError):
            timing_log_offset = 0
    if use_wheel_measure:
        ok, output, error = interaction.run_helper(helper, [
            "wheel-measure", str(pid), unit, str(delta), str(window_id), str(frame_dir),
            ",".join(str(delay) for delay in delays),
            str(scroll_event_timing_path), str(scroll_event_poll_timeout_ms),
        ], helper_timeout)
        if not ok:
            return [], None, error
        try:
            evidence = parse_wheel_measure_capture_output(output, len(delays))
        except ValueError as exc:
            return [], None, f"{exc}\nwheel-measure output:\n{bounded_wheel_measure_output(output)}"
        # A separate observer classification of what the one shared scroll
        # input established on the mach clock (Issue #427); never feeds back
        # into `evidence`/the frames below, so it cannot change what
        # evaluate_lines_coast/evaluate_pixels judge from this same capture.
        scroll_event_observation = scroll_event_observation_module.assess_wheel_measurement(
            scroll_event_observation_module.parse_wheel_measure_output(output, len(delays))
        )
        scroll_event_observation["wheel_measure_output_excerpt"] = bounded_wheel_measure_output(output)
        scroll_event_observation["scroll_event_timing_log_delta"] = scroll_event_timing_log_delta(
            scroll_event_timing_path, timing_log_offset)
        if evidence["display_response_error"] is not None:
            scroll_event_observation["display_response_error"] = evidence["display_response_error"]
    else:
        capture_command = "wheel-capture-timed" if use_timed_wheel_capture else "wheel-capture"
        capture_args = [
            capture_command, str(pid), unit, str(delta), str(window_id), str(frame_dir),
            ",".join(str(delay) for delay in delays),
        ]
        if use_timed_wheel_capture:
            capture_args.extend((str(scroll_event_timing_path), str(scroll_event_poll_timeout_ms)))
        ok, output, error = interaction.run_helper(helper, capture_args, helper_timeout)
        if not ok:
            return [], None, error
        try:
            evidence = parse_scroll_capture_helper_output(output, len(delays))
        except ValueError as exc:
            return [], None, str(exc)
        if use_timed_wheel_capture:
            scroll_event_observation = scroll_event_observation_module.assess_wheel_capture_timing(
                scroll_event_observation_module.parse_wheel_capture_timing_output(output)
            )
            scroll_event_observation["wheel_capture_timing_output_excerpt"] = bounded_wheel_measure_output(output)
            scroll_event_observation["scroll_event_timing_log_delta"] = scroll_event_timing_log_delta(
                scroll_event_timing_path, timing_log_offset)
    frames = []
    for index, elapsed in enumerate(evidence["frame_elapsed_ms"]):
        path = frame_dir / f"frame-{index:02d}.png"
        lines, text, ocr_error = capture_ocr(interaction, helper, path, helper_timeout)
        if ocr_error:
            return frames, scroll_event_observation, ocr_error
        frames.append({"path": str(path),
                       "capture_started_elapsed_ms": evidence["frame_capture_started_ms"][index],
                       "elapsed_ms": elapsed,
                       "capture_completed_elapsed_ms": evidence["frame_capture_completed_ms"][index],
                       "event_route": evidence["event_route"],
                       "visible_lines": lines, "recognized_text": text})
    if use_wheel_measure:
        display_responses = []
        if evidence["display_response_error"] is None:
            for index, response in enumerate(evidence["display_responses"]):
                response_path = frame_dir / f"display-response-{index:02d}.png"
                if response.get("image_path") != str(response_path):
                    return frames, scroll_event_observation, "WindowServer response image path does not match its sample index"
                response_lines, response_text, response_error = capture_ocr(
                    interaction, helper, response_path, helper_timeout)
                if response_error:
                    return frames, scroll_event_observation, response_error
                display_responses.append({
                    **response,
                    "path": str(response_path),
                    "visible_lines": response_lines,
                    "recognized_text": response_text,
                })
        scroll_event_observation["window_server_display_responses"] = display_responses
    return frames, scroll_event_observation, None


def capture_single(interaction, module, env, config, helper, window_id: str,
                   run_dir: Path, label: str, helper_timeout: float) -> tuple[dict, Optional[list[int]], str]:
    capture = interaction.capture_named(module, env, config, window_id, run_dir, label)
    if capture["result"] != "pass":
        return capture, None, ""
    image_path = run_dir / f"{label}.png"
    lines, text, error = capture_ocr(interaction, helper, image_path, helper_timeout)
    if error:
        capture["result"] = "blocked"
        capture["reason"] = f"OCR に失敗した: {error}"
        return capture, lines, error
    return capture, lines, text


def move_to_document_top(interaction, module, env, config, helper, pid: int,
                         window_id: str, run_dir: Path, label: str,
                         helper_timeout: float) -> tuple[dict, Optional[list[int]], str]:
    ok, _output, error = interaction.run_helper(helper, ["move-doc-start", str(pid)], helper_timeout)
    if not ok:
        return step(label, "blocked", error or "文書先頭への移動を確認できない"), None, ""
    # The caret at the start of the fixture overlays the first `L` in
    # `LINE 001`; hosted Vision then recognized that glyph as `K`. Move the
    # caret to the end of the known eight-character first line before OCR,
    # without changing document text or scroll position.
    ok, _output, error = interaction.run_helper(
        helper, ["move-caret", str(pid), "right", "8"], helper_timeout)
    if not ok:
        return step(label, "blocked", error or "OCR前に先頭行からカーソルを移動できない"), None, ""
    capture, lines, text = capture_single(
        interaction, module, env, config, helper, window_id, run_dir, label, helper_timeout)
    if capture["result"] != "pass":
        return step(label, "blocked", capture.get("reason") or "文書先頭の画面を取得できない",
                    visible_lines=lines, recognized_text=text), lines, text
    if not lines or min(lines) != 1:
        return step(label, "blocked", "文書先頭への移動後に先頭行を確認できない",
                    visible_lines=lines, recognized_text=text), lines, text
    return step(label, "pass", visible_lines=lines, recognized_text=text), lines, text


def calibrate_scroll_direction(interaction, module, env, config, helper, pid: int,
                                window_id: str, scenario_dir: Path, helper_timeout: float
                                ) -> tuple[Optional[int], dict]:
    """Determine which Lines sign advances the fixture from its verified top.

    Quartz scroll deltas are affected by the host's scroll direction setting.
    Probe each sign from the document top and use observed line movement to
    choose the downward direction; never infer it from the device type.
    """
    delays = POSITIONING_FRAME_DELAYS_MS
    top_step, top_lines, _top_text = move_to_document_top(
        interaction, module, env, config, helper, pid, window_id,
        scenario_dir, "direction-calibration-top", helper_timeout)
    if top_step["result"] != "pass":
        return None, step(
            "scroll_direction_calibration", "blocked", top_step.get("reason"),
            initial_top_step=top_step, initial_top_visible_lines=top_lines,
            initial_top_recognized_text=_top_text)
    baseline = min(top_lines) if top_lines else None
    probes = []
    downward_sign = None
    for sign, label in ((1, "positive"), (-1, "negative")):
        if sign < 0:
            top_step, top_lines, _top_text = move_to_document_top(
                interaction, module, env, config, helper, pid, window_id,
                scenario_dir, "direction-calibration-reset", helper_timeout)
            if top_step["result"] != "pass":
                probes.append({
                    "sign": sign, "result": "blocked", "reason": top_step.get("reason"),
                    "reset_step": top_step, "reset_visible_lines": top_lines,
                    "reset_recognized_text": _top_text,
                })
                break
            baseline = min(top_lines) if top_lines else None

        frames, _observation, error = capture_frames(
            interaction, module, env, config, helper, pid, window_id,
            scenario_dir / f"direction-probe-{label}", "lines", sign * POSITIONING_LINES_DELTA,
            delays, helper_timeout)
        if error:
            probes.append({"sign": sign, "result": "blocked", "reason": error})
            break
        offsets = [first_visible(frame) for frame in frames]
        moved_down = baseline is not None and any(
            offset is not None and offset > baseline for offset in offsets)
        probes.append({"sign": sign, "result": "observed", "offsets": offsets,
                       "moved_down_from_top": moved_down})
        if moved_down:
            downward_sign = sign
            break

    reset_step, reset_lines, reset_text = move_to_document_top(
        interaction, module, env, config, helper, pid, window_id,
        scenario_dir, "direction-calibration-final-reset", helper_timeout)
    if reset_step["result"] != "pass":
        return None, step("scroll_direction_calibration", "blocked", reset_step.get("reason"),
                          downward_sign=downward_sign, probes=probes,
                          reset_step=reset_step, reset_visible_lines=reset_lines,
                          reset_recognized_text=reset_text)
    if downward_sign is None:
        return None, step(
            "scroll_direction_calibration", "blocked",
            "正負どちらのLines入力でも文書先頭から下方向への移動を確認できない",
            probes=probes, reset_visible_lines=reset_lines)
    return downward_sign, step(
        "scroll_direction_calibration", "pass", downward_sign=downward_sign,
        direction="down", probes=probes, reset_visible_lines=reset_lines)


def position_document_midpoint(interaction, module, env, config, helper, pid: int,
                               window_id: str, scenario_dir: Path, helper_timeout: float,
                               downward_sign: int, label: str
                               ) -> tuple[dict, Optional[list[int]], str]:
    # This only establishes a starting location, so keep it on the regular
    # wheel-capture path that direction calibration already exercised. The
    # timed display stream is reserved for the actual Lines/Pixels acceptance
    # inputs below; positioning itself is never acceptance evidence.
    top_step, top_lines, _top_text = move_to_document_top(
        interaction, module, env, config, helper, pid, window_id,
        scenario_dir, f"{label}-top", helper_timeout)
    if top_step["result"] != "pass":
        return step(label, "blocked", top_step.get("reason")), None, ""
    position_frames = []
    positioning_attempts = []
    previous_first = first_visible({"visible_lines": top_lines})
    no_progress_steps = 0
    for attempt_number in range(1, POSITIONING_MAX_STEPS + 1):
        frames, observation, error = capture_frames(
            interaction, module, env, config, helper, pid, window_id,
            scenario_dir / f"{label}-scroll-{attempt_number:02d}", "lines",
            downward_sign * POSITIONING_LINES_DELTA,
            POSITIONING_FRAME_DELAYS_MS, helper_timeout,
        )
        position_frames.extend(frames)
        if error:
            failed = step(label, "blocked", error, frames=position_frames,
                          positioning_attempts=positioning_attempts)
            return attach_scroll_event_observation(failed, observation), None, ""
        if not frames:
            failed = step(label, "blocked", "中間位置の画面を取得できない",
                          frames=position_frames, positioning_attempts=positioning_attempts)
            return attach_scroll_event_observation(failed, observation), None, ""

        final_frame = frames[-1]
        lines = final_frame.get("visible_lines") or []
        text = final_frame.get("recognized_text") or ""
        first = min(lines) if lines else None
        last = max(lines) if lines else None
        positioning_attempts.append({
            "attempt": attempt_number,
            "visible_lines": lines,
            "recognized_text": text,
            "scroll_event_observation": observation,
        })
        if first is None or last is None:
            failed = step(label, "blocked", "スクロール後の可視行番号を読み取れない",
                          visible_lines=lines, recognized_text=text, frames=position_frames,
                          positioning_attempts=positioning_attempts)
            return attach_scroll_event_observation(failed, observation), None, text
        if midpoint_band_is_visible(lines):
            positioned = step(label, "pass", visible_lines=lines, recognized_text=text,
                              frames=position_frames, positioning_attempts=positioning_attempts)
            return attach_scroll_event_observation(positioned, observation), lines, text
        if last >= LINE_COUNT:
            failed = step(label, "blocked", "位置合わせ入力が文書末尾を越え、中間位置を確認できない",
                          visible_lines=lines, recognized_text=text, frames=position_frames,
                          positioning_attempts=positioning_attempts)
            return attach_scroll_event_observation(failed, observation), lines, text

        no_progress_steps = no_progress_steps + 1 if first <= previous_first else 0
        previous_first = first
        if no_progress_steps >= 3:
            failed = step(label, "blocked", "較正済みの小さいLines入力を3回送り、文書の移動を確認できない",
                          visible_lines=lines, recognized_text=text, frames=position_frames,
                          positioning_attempts=positioning_attempts)
            return attach_scroll_event_observation(failed, observation), lines, text

    failed = step(label, "blocked", "慣性検査の開始位置を文書中央付近に置けない",
                  visible_lines=lines, recognized_text=text, frames=position_frames,
                  positioning_attempts=positioning_attempts)
    return attach_scroll_event_observation(failed, observation), lines, text


def compare_wheel_measurement_paths(interaction, module, env, config, helper, pid: int,
                                    window_id: str, scenario_dir: Path, helper_timeout: float,
                                    downward_sign: int, timing_path: Path,
                                    observation_module: Optional[object]) -> dict:
    """Run both capture helpers from the same verified document-top state."""
    comparisons = []
    cases = (
        ("lines", "lines", downward_sign * 8, FRAME_DELAYS_MS),
        ("pixels", "pixels", downward_sign * 180, PIXELS_FRAME_DELAYS_MS),
    )
    for label, unit, delta, delays in cases:
        modes = {}
        for mode in ("wheel_capture", "wheel_measure"):
            baseline_step, baseline_lines, baseline_text = move_to_document_top(
                interaction, module, env, config, helper, pid, window_id,
                scenario_dir, f"compare-{label}-{mode}-baseline", helper_timeout)
            if baseline_step["result"] != "pass" or not baseline_lines:
                return step(
                    "wheel_measurement_path_comparison", "blocked",
                    baseline_step.get("reason") or f"{label} {mode} の開始位置を確認できない",
                    comparisons=comparisons,
                    failed_baseline={"unit": unit, "mode": mode,
                                     "step": baseline_step, "visible_lines": baseline_lines,
                                     "recognized_text": baseline_text},
                )
            kwargs = {}
            if mode == "wheel_capture":
                kwargs = {
                    "scroll_event_timing_path": timing_path,
                    "scroll_event_observation_module": observation_module,
                    "capture_scroll_event_timing": True,
                }
            else:
                kwargs = {
                    "scroll_event_timing_path": timing_path,
                    "scroll_event_observation_module": observation_module,
                }
            frames, observation, error = capture_frames(
                interaction, module, env, config, helper, pid, window_id,
                scenario_dir / f"compare-{label}-{mode}", unit, delta, delays,
                helper_timeout, **kwargs)
            if error:
                return step(
                    "wheel_measurement_path_comparison", "blocked", error,
                    comparisons=comparisons,
                    failed_capture={"unit": unit, "mode": mode,
                                    "baseline_visible_lines": baseline_lines,
                                    "scroll_event_observation": observation},
                )
            modes[mode] = {
                "baseline_visible_lines": baseline_lines,
                "baseline_text": baseline_text,
                "frames": frames,
                "response": evaluate_first_response(
                    min(baseline_lines), frames,
                    f"{label}_{mode}_first_response_80ms"),
                "scroll_event_observation": observation,
            }

        same_start_lines = (
            modes["wheel_capture"]["baseline_visible_lines"]
            == modes["wheel_measure"]["baseline_visible_lines"]
        )
        classified = classify_measurement_path_comparison(
            modes["wheel_capture"]["response"],
            modes["wheel_measure"]["response"],
            same_start_lines,
        )
        comparisons.append({
            "unit": unit,
            "delta": delta,
            "delays_ms": list(delays),
            "same_document_position": same_start_lines,
            "wheel_capture": modes["wheel_capture"],
            "wheel_measure": modes["wheel_measure"],
            **classified,
        })
    complete = all(item["result"] == "pass" for item in comparisons)
    return step(
        "wheel_measurement_path_comparison", "pass" if complete else "blocked",
        None if complete else "同一位置・同一入力で両計測経路を比較できない",
        comparisons=comparisons,
        note="この比較は計測経路の診断であり、製品受入判定はwheel-captureの画面画像と別scenarioで行う",
    )


def run_scroll_behavior_checks(interaction, module, env, config, helper, pid: int,
                               window_id: str, scenario_dir: Path,
                               helper_timeout: float, downward_sign: int) -> list[dict]:
    steps: list[dict] = []
    position, position_lines, _position_text = position_document_midpoint(
        interaction, module, env, config, helper, pid, window_id, scenario_dir,
        helper_timeout, downward_sign, "lines-positioning")
    steps.append(position)
    if position["result"] != "pass":
        steps.extend(skipped(name, "文書中央の検査開始位置を確認できなかった") for name in (
            "lines_coast", "lines_first_response_80ms", "direction_reversal",
            "document_edges", "pixels_direct_follow", "pixels_first_response_80ms"))
        return steps

    baseline = min(position_lines) if position_lines else None
    frames, _scroll_event_observation, error = capture_frames(
        interaction, module, env, config, helper, pid, window_id,
        scenario_dir / "lines-coast", "lines", downward_sign * 8,
        FRAME_DELAYS_MS, helper_timeout,
    )
    if error:
        steps.append(attach_scroll_event_observation(
            step("lines_coast", "blocked", error), None))
        steps.append(step("lines_first_response_80ms", "blocked", error))
        time.sleep(FRAME_DELAYS_MS[-1] / 1000)
    else:
        steps.append(attach_scroll_event_observation(
            evaluate_lines_coast(baseline, frames), None))
        steps.append(evaluate_first_response(baseline, frames, "lines_first_response_80ms"))

    reversal_baseline_capture, reversal_baseline_lines, reversal_baseline_text = capture_single(
        interaction, module, env, config, helper, window_id,
        scenario_dir, "reversal-baseline", helper_timeout)
    reversal_baseline = min(reversal_baseline_lines) if reversal_baseline_lines else None
    reversal_frames, pre_frame, error = capture_frames(
        interaction, module, env, config, helper, pid, window_id,
        scenario_dir / "direction-reversal",
        "lines", downward_sign * 8, REVERSE_FRAME_DELAYS_MS, helper_timeout,
        reverse_delta=downward_sign * -12, baseline=reversal_baseline)
    selected = pre_frame.get("selected") if pre_frame else None
    reversal_pre = selected["value"] if selected else None
    if reversal_baseline_capture["result"] != "pass":
        reversal_step = step("direction_reversal", "blocked",
                             reversal_baseline_capture.get("reason") or "反転基準画面を取得できない")
    elif error:
        reversal_step = step("direction_reversal", "blocked", error)
    elif pre_frame is not None and pre_frame.get("candidates") and selected is None:
        # None of the pre-reversal candidate frames captured the old-direction
        # motion inside the measured inertia window. This is insufficient
        # observation, not a product failure.
        reversal_step = step(
            "direction_reversal", "blocked",
            "135ms窓内で撮影した候補画像のいずれにも反転前の旧方向への移動が写っていない",
            baseline=reversal_baseline, candidates=pre_frame.get("candidates"),
            event_route=pre_frame.get("event_route"),
            initial_to_reverse_event_ms=pre_frame.get("initial_to_reverse_event_ms"),
        )
    else:
        reversal_step = evaluate_reversal(
            reversal_baseline, reversal_pre, reversal_frames,
            pre_frame.get("initial_to_reverse_event_ms") if pre_frame else None,
            event_route=pre_frame.get("event_route") if pre_frame else None,
            pre_reverse_capture_completed_after_initial_ms=(
                selected.get("capture_completed_after_initial_ms") if selected else None
            ),
        )
    if pre_frame:
        reversal_step["event_route"] = pre_frame.get("event_route")
        reversal_step["pre_reverse_candidates"] = pre_frame.get("candidates")
        reversal_step["pre_reverse_capture_after_initial_ms"] = (
            selected.get("capture_started_after_initial_ms") if selected else None
        )
    reversal_step["baseline_visible_lines"] = reversal_baseline_lines
    reversal_step["baseline_text"] = reversal_baseline_text
    steps.append(reversal_step)

    top_reset, _top_lines, _top_text = move_to_document_top(
        interaction, module, env, config, helper, pid, window_id,
        scenario_dir, "top-edge-start", helper_timeout)
    if top_reset["result"] != "pass":
        top_step = step("document_top_edge", "blocked", top_reset.get("reason"))
        bottom_step = skipped("document_bottom_edge", "文書先頭を確認できず、端の検査を開始できなかった")
    else:
        top_frames, _pre, top_error = capture_frames(
            interaction, module, env, config, helper, pid, window_id,
            scenario_dir / "top-edge", "lines", -downward_sign * 1000,
            (0, 48, 96, 144, 200, 260), helper_timeout)
        top_step = (step("document_top_edge", "blocked", top_error) if top_error else
                    evaluate_document_edge(top_frames, "top"))
        bottom_frames, _pre, bottom_error = capture_frames(
            interaction, module, env, config, helper, pid, window_id,
            scenario_dir / "bottom-edge", "lines", downward_sign * 1200,
            (0, 48, 96, 144, 200, 260), helper_timeout)
        bottom_step = (step("document_bottom_edge", "blocked", bottom_error) if bottom_error else
                       evaluate_document_edge(bottom_frames, "bottom"))
    edge_results = {top_step["result"], bottom_step["result"]}
    edge_result = (
        "blocked" if "blocked" in edge_results else
        "fail" if "fail" in edge_results else
        "pass"
    )
    steps.append(step(
        "document_edges", edge_result,
        None if edge_result == "pass" else "先頭または末尾で端の表示を維持できない",
        top=top_step, bottom=bottom_step,
    ))

    pixel_position, before_lines, before_text = position_document_midpoint(
        interaction, module, env, config, helper, pid, window_id, scenario_dir,
        helper_timeout, downward_sign, "pixels-before")
    if pixel_position["result"] == "pass":
        before_pixel_offset = min(before_lines) if before_lines else None
        frames, _pixels_scroll_event_observation, error = capture_frames(
            interaction, module, env, config, helper, pid, window_id,
            scenario_dir / "pixels-direct-follow",
            "pixels", downward_sign * 180, PIXELS_FRAME_DELAYS_MS, helper_timeout,
        )
        pixel_step = (
            step("pixels_direct_follow", "blocked", error)
            if error else evaluate_pixels(before_pixel_offset, frames)
        )
        pixel_response_step = (
            step("pixels_first_response_80ms", "blocked", error)
            if error else evaluate_first_response(
                before_pixel_offset, frames, "pixels_first_response_80ms")
        )
        pixel_step = attach_scroll_event_observation(
            pixel_step, None)
        pixel_step["baseline_visible_lines"] = before_lines
        pixel_step["baseline_text"] = before_text
    else:
        pixel_step = attach_scroll_event_observation(
            step("pixels_direct_follow", "blocked", pixel_position.get("reason")), None)
        pixel_response_step = step(
            "pixels_first_response_80ms", "blocked", pixel_position.get("reason"))
    steps.append(pixel_step)
    steps.append(pixel_response_step)
    return steps


def run_focused_scenario(gui_validate, interaction, env, target_dir: Path, helper,
                         run_dir: Path, binary_path: Path, expected_sha: str,
                         request_id: str, startup_timeout: float, window_timeout: float,
                         helper_timeout: float, priority: dict[str, int],
                         scroll_event_observation_module: Optional[object] = None) -> dict:
    scenario_dir = run_dir / "scroll_inertia"
    scenario_dir.mkdir(parents=True, exist_ok=True)
    fixture = scenario_dir / "scroll-inertia-fixture.md"
    contents = "\n".join(f"LINE {number:03d}" for number in range(1, LINE_COUNT + 1)) + "\n"
    fixture.write_text(contents, encoding="utf-8")
    # Issue #427: Hane writes its own mach-clock scroll-timing record to this
    # path only when launched with HANE_SCROLL_EVENT_TIMING_PATH set. Version
    # 18 uses the one running process and this path to compare the same
    # Lines/Pixels inputs through wheel-capture and wheel-measure.
    scroll_event_timing_path = scenario_dir / "scroll-event-timing.log"
    config = interaction.make_config(
        gui_validate, workspace_dir=target_dir, scenario="scroll-inertia-focused",
        expected_sha=expected_sha, request_id=request_id, generation="1", run_dir=scenario_dir,
        fixture_path=fixture, features=["timing-probe"],
        extra_env={"HANE_SCROLL_EVENT_TIMING_PATH": str(scroll_event_timing_path)},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
    )
    steps: list[dict] = []
    holder = {"process": None}
    try:
        session_steps, window_id = interaction.open_session(
            gui_validate, env, config, binary_path, holder, "before",
            helper, helper_timeout,
        )
        steps.extend(session_steps)
        pid = interaction.current_pid(holder)
        if pid is None or window_id is None:
            steps.extend(skipped(name, "launch/window discovery が pass しなかった") for name in (
                "focus_editor", "wheel_measurement_path_comparison", "lines_coast",
                "lines_first_response_80ms", "direction_reversal", "document_edges",
                "pixels_direct_follow", "pixels_first_response_80ms", "document_unchanged"))
        else:
            ok, _output, error = interaction.run_helper(helper, ["focus-editor", str(pid)], helper_timeout)
            steps.append(step("focus_editor", "pass" if ok else "blocked", None if ok else error))
            if not ok:
                steps.extend(skipped(name, "editorへのフォーカスを確認できなかった") for name in (
                    "wheel_measurement_path_comparison", "lines_coast",
                    "lines_first_response_80ms", "direction_reversal", "document_edges",
                    "pixels_direct_follow", "pixels_first_response_80ms"))
            else:
                baseline_capture, baseline_lines, baseline_text = capture_single(
                    interaction, gui_validate, env, config, helper, window_id,
                    scenario_dir, "baseline", helper_timeout)
                steps.append(step("baseline_capture", baseline_capture["result"],
                                  baseline_capture.get("reason"), visible_lines=baseline_lines,
                                  recognized_text=baseline_text))
                if baseline_capture["result"] != "pass" or not baseline_lines:
                    steps.append(step("scroll_direction_calibration", "blocked",
                                      "基準画面を取得できず、Linesの向きを確認できない"))
                    steps.extend(skipped(name, "Linesの向きを確認できなかった") for name in (
                        "wheel_measurement_path_comparison", "lines_coast",
                        "lines_first_response_80ms", "direction_reversal", "document_edges",
                        "pixels_direct_follow", "pixels_first_response_80ms"))
                else:
                    downward_sign, calibration = calibrate_scroll_direction(
                        interaction, gui_validate, env, config, helper, pid,
                        window_id, scenario_dir, helper_timeout)
                    steps.append(calibration)
                    if downward_sign is None:
                        steps.extend(skipped(name, "Linesの向きを確認できなかった") for name in (
                            "wheel_measurement_path_comparison", "lines_coast",
                            "lines_first_response_80ms", "direction_reversal", "document_edges",
                            "pixels_direct_follow", "pixels_first_response_80ms"))
                    else:
                        steps.append(compare_wheel_measurement_paths(
                            interaction, gui_validate, env, config, helper, pid,
                            window_id, scenario_dir, helper_timeout, downward_sign,
                            scroll_event_timing_path, scroll_event_observation_module))
                        steps.extend(run_scroll_behavior_checks(
                            interaction, gui_validate, env, config, helper, pid,
                            window_id, scenario_dir, helper_timeout, downward_sign))

                unchanged = fixture.read_bytes() == contents.encode("utf-8")
                steps.append(step("document_unchanged", "pass" if unchanged else "fail",
                                  None if unchanged else "スクロール検証でfixtureのbytesが変わった",
                                  fixture_path=str(fixture), sha256=hashlib.sha256(fixture.read_bytes()).hexdigest()))
    finally:
        if holder.get("process") is not None:
            steps.append(interaction.close_session(gui_validate, env, holder))

    result, reasons = aggregate_acceptance_result(steps, priority)
    return {"name": "scroll_inertia", "steps": steps, "result": result,
            "reason": "。".join(reasons) if reasons else "Issue #389 のスクロールGUI確認が成功した",
            "evidence": {"fixture_path": str(fixture), "line_count": LINE_COUNT,
                         "run_dir": str(scenario_dir)}}


def main() -> int:
    target_value = os.environ.get("HANE_SCROLL_INERTIA_GUI_TARGET_DIR")
    control_value = os.environ.get("HANE_SCROLL_INERTIA_GUI_CONTROL_DIR")
    expected_sha = os.environ.get("HANE_SCROLL_INERTIA_GUI_EXPECTED_SHA", "")
    if not target_value or not control_value or not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        print("target dir, control dir and full expected SHA are required", file=sys.stderr)
        return 3

    target_dir = Path(target_value).resolve()
    control_dir = Path(control_value).resolve()
    run_dir = Path(os.environ.get("HANE_SCROLL_INERTIA_GUI_RUN_DIR")
                   or (Path(tempfile.gettempdir()) / "hane-scroll-inertia-gui")).resolve()
    request_id = os.environ.get("HANE_SCROLL_INERTIA_GUI_REQUEST_ID") or f"scroll-inertia-{os.getpid()}"
    startup_timeout = env_float("HANE_SCROLL_INERTIA_GUI_STARTUP_TIMEOUT_SECS", 15.0)
    window_timeout = env_float("HANE_SCROLL_INERTIA_GUI_WINDOW_TIMEOUT_SECS", 30.0)
    helper_timeout = env_float("HANE_SCROLL_INERTIA_GUI_HELPER_TIMEOUT_SECS", 20.0)
    run_dir.mkdir(parents=True, exist_ok=True)
    result_path = run_dir / "result.json"

    gui_validate = load_module(control_dir, "scripts/gui_validate.py", "scroll_gui_validate")
    interaction = load_module(control_dir, "scripts/hosted_gui_interaction.py", "scroll_gui_interaction")
    scroll_event_observation = load_module(
        control_dir, "scripts/aadw_scroll_event_observation.py", "scroll_event_observation"
    )
    env = gui_validate.RealEnvironment()
    priority = gui_validate.RESULT_PRIORITY
    top_steps: list[dict] = []
    scenarios: list[dict] = []
    target_info: dict = {}
    build_info: dict = {}
    started_at = env.clock.now_iso()
    helper_tmp = tempfile.TemporaryDirectory(prefix="hane-scroll-inertia-helper-")

    try:
        env.acquire_execution()
        config = interaction.make_config(
            gui_validate, workspace_dir=target_dir, scenario="scroll-inertia-preflight",
            expected_sha=expected_sha, request_id=request_id, generation="0", run_dir=run_dir / "_preflight",
            fixture_path=None, features=["timing-probe"], extra_env={},
            startup_timeout=startup_timeout, window_timeout=window_timeout,
        )
        preflight, target_info = gui_validate.do_preflight(env, config)
        top_steps.append(preflight)
        binary_path = None
        helper = None
        if preflight["result"] == "pass":
            try:
                helper = interaction.prepare_helper(
                    control_dir / "scripts" / "hosted_gui_interaction.swift", Path(helper_tmp.name)
                )
                top_steps.append(step("prepare_helper", "pass", sha256=helper.digest))
            except (OSError, subprocess.SubprocessError) as exc:
                top_steps.append(step("prepare_helper", "blocked", str(exc)))
            if helper is not None:
                build_step, binary_path, build_info = gui_validate.do_build(
                    env, config, target_info["actual_sha"])
                top_steps.append(build_step)
                if build_step["result"] != "pass":
                    binary_path = None
        else:
            top_steps.extend((skipped("prepare_helper", "preflight が pass しなかった"),
                              skipped("build", "preflight が pass しなかった")))

        if binary_path is not None and helper is not None:
            scenarios.append(run_focused_scenario(
                gui_validate, interaction, env, target_dir, helper, run_dir, binary_path,
                expected_sha, request_id, startup_timeout, window_timeout, helper_timeout,
                priority, scroll_event_observation_module=scroll_event_observation))

        values = [item["result"] for item in top_steps + scenarios if item.get("result") in priority]
        overall = min(values, key=lambda value: priority[value]) if values else "blocked"
        reasons = [item.get("reason") for item in top_steps + scenarios
                   if item.get("result") not in ("pass", "skipped") and item.get("reason")]
        reason = "。".join(reasons) if reasons else "Issue #389 のスクロールGUI確認が成功した"
        result = {
            "schema_version": SCHEMA_VERSION,
            "procedure_version": PROCEDURE_VERSION,
            "verification_kind": VERIFICATION_KIND,
            "scope_note": SCOPE_NOTE,
            "request_id": request_id,
            "started_at": started_at,
            "finished_at": env.clock.now_iso(),
            "target": target_info,
            "control": {"sha": env.git_head(control_dir)},
            "runner": {"os": os.environ.get("RUNNER_OS", ""), "arch": os.environ.get("RUNNER_ARCH", ""),
                       "macos_version": platform.mac_ver()[0], "machine": platform.machine()},
            "build": build_info,
            "top_level_steps": top_steps,
            "scenarios": scenarios,
            "overall_result": overall,
            "overall_reason": reason,
        }
        label = {"pass": "PASS", "fail": "FAIL", "blocked": "BLOCKED"}.get(overall, "BLOCKED")
        result["summary"] = f"[{label}] {PROCEDURE_VERSION} — {reason}"
        result_path.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        (run_dir / "summary.md").write_text(result["summary"] + "\n", encoding="utf-8")
        print(result["summary"])
        return EXIT_PASS if overall == "pass" else EXIT_NONPASS
    except (gui_validate.Aborted, gui_validate.EnvError, OSError, subprocess.SubprocessError,
            ValueError, RuntimeError) as exc:
        fallback = {"schema_version": SCHEMA_VERSION, "procedure_version": PROCEDURE_VERSION,
                    "verification_kind": VERIFICATION_KIND, "scope_note": SCOPE_NOTE,
                    "request_id": request_id, "overall_result": "blocked",
                    "overall_reason": str(exc), "top_level_steps": top_steps, "scenarios": scenarios}
        result_path.write_text(json.dumps(fallback, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"[BLOCKED] {PROCEDURE_VERSION} — {exc}")
        return EXIT_NONPASS
    finally:
        env.release_execution()
        helper_tmp.cleanup()


if __name__ == "__main__":
    sys.exit(main())
