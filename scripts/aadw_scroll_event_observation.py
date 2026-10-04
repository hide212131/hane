#!/usr/bin/env python3
"""Measurement-only correlation of one scroll input (Issue #427).

Distinguishes points in time on the one mach clock both the GUI helper
process and Hane's own product process read: the OS event post, Hane's
`ScrollWheelEvent` receipt, the frame paint/submission Hane committed in
response, the helper's screenshot capture start/end, and a separate
ScreenCaptureKit sample's WindowServer display time.

Hane's own timestamp is when `InputCapture::paint` ran, inside `Window::draw`
and before the platform renderer commits/presents the frame (on macOS, an
async Metal command buffer with its own completion handler) — it is not
compositor presentation. This module never relabels or infers it as one.
The helper's WindowServer display timestamp is recorded separately;
product-side presentation remains unavailable.

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
    return parsed if 0 <= parsed <= 0xFFFFFFFFFFFFFFFF else None


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
        "display_response_collection_valid": fields.get("display_response_collection_valid") == "true",
        "display_response_count": _uint(fields, "display_response_count"),
        "display_responses": [
            {
                "sample_id": _uint(fields, f"display_response_{index:02d}_sample_id"),
                "frame_status": fields.get(f"display_response_{index:02d}_frame_status"),
                "timestamp_source": fields.get(f"display_response_{index:02d}_timestamp_source"),
                "image_source": fields.get(f"display_response_{index:02d}_image_source"),
                "display_time_ticks": _uint(fields, f"display_response_{index:02d}_display_time_ticks"),
                "callback_received_ticks": _uint(fields, f"display_response_{index:02d}_callback_received_ticks"),
                "image_ready_ticks": _uint(fields, f"display_response_{index:02d}_image_ready_ticks"),
                "artifact_written_ticks": _uint(fields, f"display_response_{index:02d}_artifact_written_ticks"),
                "image_path": fields.get(f"display_response_{index:02d}_image_path"),
            }
            for index in range(min(_uint(fields, "display_response_count") or 0, 64))
        ],
        "frames": frames,
    }


def parse_wheel_capture_timing_output(output: str) -> dict:
    """Reads only the shared-clock event/receipt/paint fields from the
    regular wheel-capture helper. It deliberately does not infer compositor
    presentation from Hane's paint timestamp."""
    fields = _parse_fields(output)
    return {
        "event_route": fields.get("event_route"),
        "event_post_ticks": _uint(fields, "event_post_ticks"),
        "helper_mach_timebase_numer": _uint(fields, "mach_timebase_numer"),
        "helper_mach_timebase_denom": _uint(fields, "mach_timebase_denom"),
        "product_scroll_receipt_ticks": _uint(fields, "product_scroll_receipt_ticks"),
        "product_frame_paint_ticks": _uint(fields, "product_frame_paint_ticks"),
        "product_mach_timebase_numer": _uint(fields, "product_mach_timebase_numer"),
        "product_mach_timebase_denom": _uint(fields, "product_mach_timebase_denom"),
    }


def assess_wheel_capture_timing(record: dict) -> dict:
    """Classifies whether the existing screenshot helper established the
    event-post -> product-receipt -> product-paint chain on a shared mach
    clock. This is timing diagnosis only, never Issue #389 acceptance."""
    if record.get("event_route") != "cghidEventTap":
        return _unavailable("OSイベント経路がcghidEventTapではない。")
    numer = record.get("helper_mach_timebase_numer")
    denom = record.get("helper_mach_timebase_denom")
    product_numer = record.get("product_mach_timebase_numer")
    product_denom = record.get("product_mach_timebase_denom")
    event_ticks = record.get("event_post_ticks")
    receipt_ticks = record.get("product_scroll_receipt_ticks")
    paint_ticks = record.get("product_frame_paint_ticks")
    if (type(event_ticks) is not int or type(receipt_ticks) is not int
            or type(paint_ticks) is not int or not numer or not denom
            or numer > 0xFFFFFFFF or denom > 0xFFFFFFFF):
        return _unavailable("イベント送出・Hane受信・描画時刻のいずれかが欠落している。")
    if (not product_numer or not product_denom
            or product_numer > 0xFFFFFFFF or product_denom > 0xFFFFFFFF
            or numer != product_numer or denom != product_denom):
        return _unavailable("ヘルパーとHaneのmach timebaseが一致しない。")

    event_ms = _ticks_to_ms(event_ticks, numer, denom)
    receipt_ms = _ticks_to_ms(receipt_ticks, numer, denom)
    paint_ms = _ticks_to_ms(paint_ticks, numer, denom)
    if any(value is None or not math.isfinite(value) for value in (event_ms, receipt_ms, paint_ms)):
        return _unavailable("共有mach時刻をミリ秒へ変換できない。")
    ordered = event_ticks <= receipt_ticks <= paint_ticks
    stages = {
        "event_post_ms": event_ms,
        "scroll_receipt_ms": receipt_ms,
        "frame_paint_ms": paint_ms,
        "event_to_receipt_ms": receipt_ms - event_ms,
        "receipt_to_paint_ms": paint_ms - receipt_ms,
        "event_to_paint_ms": paint_ms - event_ms,
    }
    return {
        "observation": "observed_ordered" if ordered else "observed_disordered",
        "reason": None if ordered else "イベント受信と描画の時刻が因果順になっていない。",
        "stages_ms": stages,
        "clock_consistent": True,
        "window_server_display_observation": "unavailable",
        "presentation_observation": "unavailable",
    }


def _ticks_to_ms(ticks: Optional[int], numer: Optional[int], denom: Optional[int]) -> Optional[float]:
    if ticks is None or not numer or not denom:
        return None
    try:
        return ticks * numer / denom / 1_000_000.0
    except OverflowError:
        return None


def _unavailable(reason: str) -> dict:
    return {
        "observation": "unavailable",
        "reason": reason,
        "stages_ms": {},
        "clock_consistent": False,
        "window_server_display_observation": "unavailable",
        # Hane's product process does not observe its WindowServer display
        # time. Preserve that fact separately from the helper observation.
        "presentation_observation": "unavailable",
    }


def assess_wheel_measurement(record: dict) -> dict:
    """Classifies one `parse_wheel_measure_output` record. Never infers a
    missing stage from the others, and never returns a product pass/fail
    verdict: only whether the stages were established, and if so, whether
    Hane's own event post → ScrollWheelEvent receipt → frame paint/submission
    chain appeared on the clock in that causal order.

    The helper's screenshot captures are scheduled at fixed delays from the
    event post, independent of when Hane actually paints; an early capture
    (e.g. the 0ms-delay frame) legitimately starting before Hane's paint is
    not a clock disorder, and a later one starting after paint is not proof
    the capture shows the presented frame. Neither is compared against paint
    to decide `observed_ordered` vs. `observed_disordered` — each frame's raw
    timestamps are preserved alongside an informational
    `captured_before_paint` classification instead, so a caller can see which
    captures preceded Hane's response without that distinction ever being
    read back as a pass/fail or as compositor presentation.

    `product_frame_paint_ticks` is Hane's paint/submission time, not
    compositor presentation. The helper's WindowServer display timestamp is
    reported separately and never copied into the product-side field;
    `presentation_observation` remains `"unavailable"` for product-side
    presentation."""
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
    if (record.get("event_post_ticks") is None or not numer or not denom
            or numer > 0xFFFFFFFF or denom > 0xFFFFFFFF or not have_frames):
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

    event_ticks = record["event_post_ticks"]
    displays = record.get("display_responses")
    if (record.get("display_response_collection_valid") is not True
            or not isinstance(displays, list) or not 1 <= len(displays) <= 64
            or record.get("display_response_count") != len(displays)):
        return _unavailable("WindowServer表示サンプル群が欠落、不完全、または上限を超えている。")
    display_stages = []
    seen_sample_ids: set[int] = set()
    previous_display_ticks = event_ticks - 1
    for display in displays:
        display_ticks = display.get("display_time_ticks")
        callback_ticks = display.get("callback_received_ticks")
        image_ready_ticks = display.get("image_ready_ticks")
        artifact_written_ticks = display.get("artifact_written_ticks")
        sample_id = display.get("sample_id")
        if (display.get("frame_status") != "complete"
                or display.get("timestamp_source") != "SCStreamFrameInfo.displayTime"
                or display.get("image_source") != "same_CMSampleBuffer"
                or type(sample_id) is not int or sample_id <= 0 or sample_id in seen_sample_ids
                or any(type(value) is not int for value in (
                    display_ticks, callback_ticks, image_ready_ticks, artifact_written_ticks))
                or not isinstance(display.get("image_path"), str) or not display["image_path"]):
            return _unavailable("WindowServer表示時刻と同一サンプル画像のメタデータが欠落または不正。")
        if (display_ticks < event_ticks or callback_ticks < event_ticks
                or display_ticks <= previous_display_ticks
                or image_ready_ticks < callback_ticks
                or artifact_written_ticks < max(image_ready_ticks, display_ticks)):
            return _unavailable("WindowServer表示サンプルの時刻がイベント後に成立していない。")
        stage = {
            "sample_id": sample_id,
            "window_server_display_ms": _ticks_to_ms(display_ticks, numer, denom),
            "callback_received_ms": _ticks_to_ms(callback_ticks, numer, denom),
            "image_ready_ms": _ticks_to_ms(image_ready_ticks, numer, denom),
            "artifact_written_ms": _ticks_to_ms(artifact_written_ticks, numer, denom),
            "frame_status": display["frame_status"],
            "timestamp_source": display["timestamp_source"],
            "image_source": display["image_source"],
            "image_path": display["image_path"],
        }
        if any(value is None or not math.isfinite(value) for key, value in stage.items()
               if key.endswith("_ms")):
            return _unavailable("WindowServer表示サンプル時刻をミリ秒へ変換できない。")
        display_stages.append(stage)
        seen_sample_ids.add(sample_id)
        previous_display_ticks = display_ticks

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
    ), *(value for stage in display_stages for key, value in stage.items() if key.endswith("_ms"))]
    if any(value is None or not math.isfinite(value) for value in all_ms):
        return _unavailable("mach時刻をミリ秒へ変換できない。")

    # Informational only: whether each capture's raw start time preceded
    # Hane's paint. An early (e.g. 0ms-delay) frame starting before paint is
    # expected, not a disorder; this is never used to decide the
    # ordered/disordered verdict below, and never implies the frame matches
    # the presented frame.
    for frame in frame_stages:
        frame["captured_before_paint"] = frame["capture_started_ms"] < paint_ms

    stages_ms = {
        "event_post_ms": event_post_ms,
        "scroll_receipt_ms": receipt_ms,
        "frame_paint_ms": paint_ms,
        "window_server_display_responses": display_stages,
        "frames_ms": frame_stages,
    }
    ordered = event_post_ms <= receipt_ms <= paint_ms
    if not ordered:
        return {
            "observation": "observed_disordered",
            "reason": "event post → ScrollWheelEvent受信 → フレーム描画の順序が成立していない。",
            "stages_ms": stages_ms,
            "clock_consistent": True,
            "window_server_display_observation": "observed",
            "presentation_observation": "unavailable",
        }
    return {
        "observation": "observed_ordered",
        "reason": None,
        "stages_ms": stages_ms,
        "clock_consistent": True,
        "window_server_display_observation": "observed",
        "presentation_observation": "unavailable",
    }
