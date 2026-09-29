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
PROCEDURE_VERSION = "hosted-scroll-inertia/9"
VERIFICATION_KIND = "scroll_inertia_focused"
SCOPE_NOTE = (
    "Issue #389 に限定した focused GUI evidence。Lines の初回応答・解放後の余韻と減速・"
    "逆方向入力への切替、文書先頭/末尾のクランプ、Pixels の直接追従と安定を実画面で確認する。"
    "入力イベントは ScrollDelta 相当の Lines / Pixels を明示して発生させ、端末種別は推測しない。"
    "画面取得は入力イベントと同一mach時計で開始・完了を計り、Lines慣性窓内の135ms以内に応答が見えた画像だけで判定する。"
    "Pixelsは応答付近を連続して撮影し、反転入力は複数の旧方向候補画面を撮影した直後に送り、OCRはその後に行って慣性窓を消費しない。"
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
PRE_REVERSE_PROBE_DELAYS_MS = (64, 96, 120)
LINES_INERTIA_WINDOW_MS = 135.0
# The issue specifies a 100–150ms coast, but no separate 80ms latency target.
# Use the 135ms measurement window for the first completed screenshot too, so
# the capture callback remains inside the bounded response period.
VISIBLE_RESPONSE_WINDOW_MS = LINES_INERTIA_WINDOW_MS


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


def skipped(name: str, reason: str) -> dict:
    return step(name, "skipped", reason)


def visible_lines(text: str) -> list[int]:
    return sorted({int(value) for value in re.findall(r"\bLINE\s+(\d+)\b", text, re.I)})


def first_visible(frame: dict) -> Optional[int]:
    lines = frame.get("visible_lines", [])
    return min(lines) if lines else None


def last_visible(frame: dict) -> Optional[int]:
    lines = frame.get("visible_lines", [])
    return max(lines) if lines else None


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
    continued = any(after > before for before, after in zip(offsets, offsets[1:]))
    monotonic = all(after >= before - 1 for before, after in zip(offsets, offsets[1:]))
    early_start = first_response_index if first_response_index is not None else 0
    early_elapsed = max(1.0, times[4] - times[early_start])
    late_elapsed = max(1.0, times[-1] - times[4])
    early_rate = max(0, offsets[4] - offsets[early_start]) / early_elapsed
    late_rate = max(0, offsets[-1] - offsets[4]) / late_elapsed
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
                   ) -> tuple[list[dict], Optional[dict], Optional[str]]:
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
    ok, output, error = interaction.run_helper(helper, [
        "wheel-capture", str(pid), unit, str(delta), str(window_id), str(frame_dir),
        ",".join(str(delay) for delay in delays),
    ], helper_timeout)
    if not ok:
        return [], None, error
    try:
        evidence = parse_scroll_capture_helper_output(output, len(delays))
    except ValueError as exc:
        return [], None, str(exc)
    frames = []
    for index, elapsed in enumerate(evidence["frame_elapsed_ms"]):
        path = frame_dir / f"frame-{index:02d}.png"
        lines, text, ocr_error = capture_ocr(interaction, helper, path, helper_timeout)
        if ocr_error:
            return frames, None, ocr_error
        frames.append({"path": str(path),
                       "capture_started_elapsed_ms": evidence["frame_capture_started_ms"][index],
                       "elapsed_ms": elapsed,
                       "capture_completed_elapsed_ms": evidence["frame_capture_completed_ms"][index],
                       "event_route": evidence["event_route"],
                       "visible_lines": lines, "recognized_text": text})
    return frames, None, None


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


def run_focused_scenario(gui_validate, interaction, env, target_dir: Path, helper,
                         run_dir: Path, binary_path: Path, expected_sha: str,
                         request_id: str, startup_timeout: float, window_timeout: float,
                         helper_timeout: float, priority: dict[str, int]) -> dict:
    scenario_dir = run_dir / "scroll_inertia"
    scenario_dir.mkdir(parents=True, exist_ok=True)
    fixture = scenario_dir / "scroll-inertia-fixture.md"
    contents = "\n".join(f"LINE {number:03d}" for number in range(1, LINE_COUNT + 1)) + "\n"
    fixture.write_text(contents, encoding="utf-8")
    config = interaction.make_config(
        gui_validate, workspace_dir=target_dir, scenario="scroll-inertia-focused",
        expected_sha=expected_sha, request_id=request_id, generation="1", run_dir=scenario_dir,
        fixture_path=fixture, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
    )
    steps: list[dict] = []
    holder = {"process": None}
    try:
        session_steps, window_id = interaction.open_session(
            gui_validate, env, config, binary_path, holder, "before"
        )
        steps.extend(session_steps)
        pid = interaction.current_pid(holder)
        if pid is None or window_id is None:
            steps.extend(skipped(name, "launch/window discovery が pass しなかった") for name in (
                "focus_editor", "lines_coast", "direction_reversal", "document_edges",
                "pixels_direct_follow", "document_unchanged"))
        else:
            ok, _output, error = interaction.run_helper(helper, ["focus-editor", str(pid)], helper_timeout)
            steps.append(step("focus_editor", "pass" if ok else "blocked", None if ok else error))
            if not ok:
                steps.extend(skipped(name, "editorへのフォーカスを確認できなかった") for name in (
                    "lines_coast", "direction_reversal", "document_edges", "pixels_direct_follow"))
            else:
                baseline_capture, baseline_lines, baseline_text = capture_single(
                    interaction, gui_validate, env, config, helper, window_id,
                    scenario_dir, "baseline", helper_timeout)
                steps.append(step("baseline_capture", baseline_capture["result"],
                                  baseline_capture.get("reason"), visible_lines=baseline_lines,
                                  recognized_text=baseline_text))
                baseline = min(baseline_lines) if baseline_lines else None

                frames, _pre, error = capture_frames(
                    interaction, gui_validate, env, config, helper, pid, window_id,
                    scenario_dir / "lines-coast",
                    "lines", -8, FRAME_DELAYS_MS, helper_timeout)
                if error:
                    steps.append(step("lines_coast", "blocked", error))
                    time.sleep(FRAME_DELAYS_MS[-1] / 1000)
                else:
                    steps.append(evaluate_lines_coast(baseline, frames))

                reversal_baseline_capture, reversal_baseline_lines, reversal_baseline_text = capture_single(
                    interaction, gui_validate, env, config, helper, window_id,
                    scenario_dir, "reversal-baseline", helper_timeout)
                reversal_baseline = min(reversal_baseline_lines) if reversal_baseline_lines else None
                reversal_frames, pre_frame, error = capture_frames(
                    interaction, gui_validate, env, config, helper, pid, window_id,
                    scenario_dir / "direction-reversal",
                    "lines", -8, REVERSE_FRAME_DELAYS_MS, helper_timeout,
                    reverse_delta=12, baseline=reversal_baseline)
                selected = pre_frame.get("selected") if pre_frame else None
                reversal_pre = selected["value"] if selected else None
                if reversal_baseline_capture["result"] != "pass":
                    reversal_step = step("direction_reversal", "blocked",
                                         reversal_baseline_capture.get("reason") or "反転基準画面を取得できない")
                elif error:
                    reversal_step = step("direction_reversal", "blocked", error)
                elif pre_frame is not None and pre_frame.get("candidates") and selected is None:
                    # None of the pre-reversal candidate frames captured the
                    # old-direction motion within the inertia window; this is
                    # insufficient observation, not a product failure.
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

                top_frames, _pre, top_error = capture_frames(
                    interaction, gui_validate, env, config, helper, pid, window_id,
                    scenario_dir / "top-edge", "lines", 1000,
                    (0, 48, 96, 144, 200, 260), helper_timeout)
                top_step = (step("document_top_edge", "blocked", top_error) if top_error else
                            evaluate_document_edge(top_frames, "top"))
                bottom_frames, _pre, bottom_error = capture_frames(
                    interaction, gui_validate, env, config, helper, pid, window_id,
                    scenario_dir / "bottom-edge", "lines", -1200,
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

                reset_ok, _output, reset_error = interaction.run_helper(
                    helper, ["wheel-event", str(pid), "lines", "1000"], helper_timeout)
                if reset_ok:
                    time.sleep(0.24)
                    before_capture, before_lines, before_text = capture_single(
                        interaction, gui_validate, env, config, helper, window_id,
                        scenario_dir, "pixels-before", helper_timeout)
                    before_pixel_offset = min(before_lines) if before_lines else None
                    frames, _pre, error = capture_frames(
                        interaction, gui_validate, env, config, helper, pid, window_id,
                        scenario_dir / "pixels-direct-follow",
                        "pixels", -180, PIXELS_FRAME_DELAYS_MS, helper_timeout)
                    pixel_step = (
                        step("pixels_direct_follow", "blocked", error)
                        if error else evaluate_pixels(before_pixel_offset, frames)
                    )
                    pixel_step["baseline_visible_lines"] = before_lines
                    pixel_step["baseline_text"] = before_text
                    if before_capture["result"] != "pass":
                        pixel_step = step("pixels_direct_follow", "blocked",
                                          before_capture.get("reason") or "Pixels前の画面を取得できない")
                    steps.append(pixel_step)
                else:
                    steps.append(step("pixels_direct_follow", "blocked", reset_error))

                unchanged = fixture.read_bytes() == contents.encode("utf-8")
                steps.append(step("document_unchanged", "pass" if unchanged else "fail",
                                  None if unchanged else "スクロール検証でfixtureのbytesが変わった",
                                  fixture_path=str(fixture), sha256=hashlib.sha256(fixture.read_bytes()).hexdigest()))
    finally:
        if holder.get("process") is not None:
            steps.append(interaction.close_session(gui_validate, env, holder))

    result = min((item["result"] for item in steps if item.get("result") in priority),
                 key=lambda value: priority[value]) if any(item.get("result") in priority for item in steps) else "blocked"
    reasons = [item.get("reason") for item in steps if item.get("result") not in ("pass", "skipped") and item.get("reason")]
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
                priority))

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
