#!/usr/bin/env python3
"""Measurement-only correlation of one scroll input (Issue #427).

Distinguishes four points in time on the one mach clock both the GUI helper
process and Hane's own product process read: the OS event post, Hane's
`ScrollWheelEvent` receipt, the frame paint/submission Hane committed in
response, and the helper's screenshot capture start/end.

Hane's own timestamp is when `InputCapture::paint` ran, inside `Window::draw`
and before the platform renderer commits/presents the frame (on macOS, an
async Metal command buffer with its own completion handler) — it is not
compositor presentation. This module never relabels or infers it as one:
compositor presentation itself is always reported as a separate, unavailable
observation.

This module never judges Issue #389's product acceptance: a well-ordered
measurement is not a pass for the 80ms/55ms/135ms scroll-inertia thresholds,
and a disordered or incomplete one is not proof of a product defect by
itself. It only reports what the two processes' own mach-clock timestamps
actually establish, and reports anything unsupported or incomplete as
measurement-unavailable rather than guessing.
"""
from __future__ import annotations

import math
from typing import Optional


def _parse_fields(output: str) -> dict[str, str]:
    fields: dict[str, str] = {}
    for line in output.splitlines():
        name, separator, value = line.partition("=")
        if separator:
            fields[name] = value
    return fields


def _uint(fields: dict[str, str], name: str) -> Optional[int]:
    value = fields.get(name)
    if value is None or value == "unavailable":
        return None
    try:
        parsed = int(value)
    except ValueError:
        return None
    return parsed if parsed >= 0 else None


def parse_wheel_measure_output(output: str, expected_frames: int) -> dict:
    """Raw fields from one `wheel-measure` helper invocation. Returns the
    parsed, still-unjudged values; callers decide availability."""
    fields = _parse_fields(output)
    frames = [
        {
            "capture_started_ticks": _uint(fields, f"frame_{index:02d}_capture_started_ticks"),
            "capture_completed_ticks": _uint(fields, f"frame_{index:02d}_capture_completed_ticks"),
        }
        for index in range(expected_frames)
    ]
    return {
        "event_route": fields.get("event_route"),
        "event_post_ticks": _uint(fields, "event_post_ticks"),
        "helper_mach_timebase_numer": _uint(fields, "mach_timebase_numer"),
        "helper_mach_timebase_denom": _uint(fields, "mach_timebase_denom"),
        "product_scroll_receipt_ticks": _uint(fields, "product_scroll_receipt_ticks"),
        "product_frame_paint_ticks": _uint(fields, "product_frame_paint_ticks"),
        "product_mach_timebase_numer": _uint(fields, "product_mach_timebase_numer"),
        "product_mach_timebase_denom": _uint(fields, "product_mach_timebase_denom"),
        "frames": frames,
    }


def _ticks_to_ms(ticks: Optional[int], numer: Optional[int], denom: Optional[int]) -> Optional[float]:
    if ticks is None or not numer or not denom:
        return None
    return ticks * numer / denom / 1_000_000.0


def _unavailable(reason: str) -> dict:
    return {
        "observation": "unavailable",
        "reason": reason,
        "stages_ms": {},
        "clock_consistent": False,
        # Compositor presentation is never measured on this clock; keep this
        # explicit even for an unavailable record so no caller can read a
        # missing key as "not yet reported" (Issue #427).
        "presentation_observation": "unavailable",
    }


def assess_wheel_measurement(record: dict) -> dict:
    """Classifies one `parse_wheel_measure_output` record. Never infers a
    missing stage from the others, and never returns a product pass/fail
    verdict: only whether the four stages were established, and if so,
    whether they appeared on the clock in the expected causal order (event
    post, then receipt, then Hane's own frame paint/submission, then the
    first screenshot capture's start). `product_frame_paint_ticks` is Hane's
    paint/submission time, not compositor presentation; this function never
    relabels or infers presentation from it, and always reports
    `presentation_observation` as `"unavailable"`."""
    if record.get("event_route") != "cghidEventTap":
        return _unavailable("OSイベント経路がcghidEventTapではない。")

    numer = record.get("helper_mach_timebase_numer")
    denom = record.get("helper_mach_timebase_denom")
    product_numer = record.get("product_mach_timebase_numer")
    product_denom = record.get("product_mach_timebase_denom")
    frames = record.get("frames") or []
    have_frames = bool(frames) and all(
        frame.get("capture_started_ticks") is not None and frame.get("capture_completed_ticks") is not None
        for frame in frames
    )
    if record.get("event_post_ticks") is None or not numer or not denom or not have_frames:
        return _unavailable("イベント送出・時計基準・画面取得のいずれかの時刻が欠落している。")

    have_product = (
        record.get("product_scroll_receipt_ticks") is not None
        and record.get("product_frame_paint_ticks") is not None
        and bool(product_numer)
        and bool(product_denom)
    )
    if not have_product:
        return _unavailable("Hane側のScrollWheelEvent受信またはフレーム描画の時刻が欠落している。")

    if numer != product_numer or denom != product_denom:
        return _unavailable("ヘルパーとHaneで観測したmach timebaseの比が一致しない。")

    event_post_ms = _ticks_to_ms(record["event_post_ticks"], numer, denom)
    receipt_ms = _ticks_to_ms(record["product_scroll_receipt_ticks"], numer, denom)
    paint_ms = _ticks_to_ms(record["product_frame_paint_ticks"], numer, denom)
    frame_stages = [
        {
            "capture_started_ms": _ticks_to_ms(frame["capture_started_ticks"], numer, denom),
            "capture_completed_ms": _ticks_to_ms(frame["capture_completed_ticks"], numer, denom),
        }
        for frame in frames
    ]
    all_ms = [event_post_ms, receipt_ms, paint_ms, *(
        value for frame in frame_stages for value in frame.values()
    )]
    if any(value is None or not math.isfinite(value) for value in all_ms):
        return _unavailable("mach時刻をミリ秒へ変換できない。")

    stages_ms = {
        "event_post_ms": event_post_ms,
        "scroll_receipt_ms": receipt_ms,
        "frame_paint_ms": paint_ms,
        "frames_ms": frame_stages,
    }
    ordered = event_post_ms <= receipt_ms <= paint_ms <= frame_stages[0]["capture_started_ms"]
    if not ordered:
        return {
            "observation": "observed_disordered",
            "reason": "event post → ScrollWheelEvent受信 → フレーム描画 → 画面取得開始の順序が成立していない。",
            "stages_ms": stages_ms,
            "clock_consistent": True,
            "presentation_observation": "unavailable",
        }
    return {
        "observation": "observed_ordered",
        "reason": None,
        "stages_ms": stages_ms,
        "clock_consistent": True,
        "presentation_observation": "unavailable",
    }
