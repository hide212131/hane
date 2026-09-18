#!/usr/bin/env python3
"""Classify Claude Code worker failures without emitting raw execution content."""

from __future__ import annotations

import json
import math
import sys
from pathlib import Path
from typing import Any

CATEGORY_USAGE = "usage_or_rate_limit"
CATEGORY_AUTH = "authentication"
CATEGORY_MAX_TURNS = "max_turns"
CATEGORY_MODEL_PROVIDER = "model_or_provider"
CATEGORY_ZERO_COST = "zero_cost_provider_or_sdk_failure"
CATEGORY_UNKNOWN = "unknown"
CATEGORY_UNAVAILABLE = "diagnostic_unavailable"


def _events(payload: Any) -> list[dict[str, Any]]:
    if isinstance(payload, list):
        return [item for item in payload if isinstance(item, dict)]
    if isinstance(payload, dict):
        return [payload]
    return []


def _error_result_text(events: list[dict[str, Any]]) -> str:
    parts: list[str] = []
    for event in events:
        if event.get("type") != "result" or event.get("is_error") is not True:
            continue
        result = event.get("result")
        if isinstance(result, str):
            parts.append(result.lower())
    return "\n".join(parts)


def _last_error_result(events: list[dict[str, Any]]) -> dict[str, Any] | None:
    for event in reversed(events):
        if event.get("type") == "result" and event.get("is_error") is True:
            return event
    return None


def _has_any(text: str, needles: tuple[str, ...]) -> bool:
    return any(needle in text for needle in needles)


def _safe_number(value: Any, *, integer: bool) -> int | float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    if not math.isfinite(float(value)) or value < 0:
        return None
    if integer:
        if not isinstance(value, int) or value > 1_000_000:
            return None
        return value
    if value > 1_000_000_000:
        return None
    return value


def _rate_limit_rejected(event: dict[str, Any]) -> bool:
    if event.get("type") != "rate_limit_event":
        return False
    info = event.get("rate_limit_info")
    return isinstance(info, dict) and info.get("status") == "rejected"


def classify_events(events: list[dict[str, Any]]) -> str:
    text = _error_result_text(events)
    result = _last_error_result(events)

    if any(_rate_limit_rejected(event) for event in events) or _has_any(
        text,
        (
            "usage limit",
            "rate limit",
            "rate_limit",
            "limit reached",
            "resets at",
            "too many requests",
            "http 429",
            "status 429",
        ),
    ):
        return CATEGORY_USAGE

    if _has_any(
        text,
        (
            "unauthorized",
            "authentication",
            "invalid oauth",
            "oauth token",
            "expired token",
            "invalid token",
            "http 401",
            "status 401",
            "forbidden",
            "http 403",
            "status 403",
        ),
    ):
        return CATEGORY_AUTH

    if result is not None:
        subtype = str(result.get("subtype", "")).lower()
        if subtype == "error_max_turns" or _has_any(
            text, ("maximum number of turns", "max turns", "max_turns")
        ):
            return CATEGORY_MAX_TURNS

    if _has_any(
        text,
        (
            "model not found",
            "not_found_error",
            "overloaded",
            "service unavailable",
            "internal server error",
            "api error",
            "provider error",
        ),
    ):
        return CATEGORY_MODEL_PROVIDER

    if result is not None:
        turns = result.get("num_turns")
        cost = result.get("total_cost_usd")
        model_usage = result.get("modelUsage")
        if (
            isinstance(turns, (int, float))
            and turns <= 1
            and isinstance(cost, (int, float))
            and cost == 0
            and (model_usage is None or model_usage == {})
        ):
            return CATEGORY_ZERO_COST

    return CATEGORY_UNKNOWN


def diagnostic(payload: Any) -> dict[str, Any]:
    events = _events(payload)
    result = _last_error_result(events)
    category = classify_events(events)

    if result is None:
        return {
            "category": category,
            "num_turns": None,
            "total_cost_usd": None,
            "model_usage": "unknown",
        }

    model_usage = result.get("modelUsage")
    if model_usage == {}:
        usage_state = "empty"
    elif isinstance(model_usage, dict):
        usage_state = "present"
    else:
        usage_state = "unknown"

    return {
        "category": category,
        "num_turns": _safe_number(result.get("num_turns"), integer=True),
        "total_cost_usd": _safe_number(
            result.get("total_cost_usd"), integer=False
        ),
        "model_usage": usage_state,
    }


def format_summary(info: dict[str, Any], *, execution_file: str) -> str:
    return (
        "Claude failure diagnostic: "
        f"category={info['category']} "
        f"num_turns={info['num_turns']} "
        f"total_cost_usd={info['total_cost_usd']} "
        f"model_usage={info['model_usage']} "
        f"execution_file={execution_file}"
    )


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(
            format_summary(
                {
                    "category": CATEGORY_UNAVAILABLE,
                    "num_turns": None,
                    "total_cost_usd": None,
                    "model_usage": "unknown",
                },
                execution_file="missing",
            )
        )
        return 0

    path = Path(argv[1])
    if not path.is_file():
        print(
            format_summary(
                {
                    "category": CATEGORY_UNAVAILABLE,
                    "num_turns": None,
                    "total_cost_usd": None,
                    "model_usage": "unknown",
                },
                execution_file="missing",
            )
        )
        return 0

    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        print(
            format_summary(
                {
                    "category": CATEGORY_UNAVAILABLE,
                    "num_turns": None,
                    "total_cost_usd": None,
                    "model_usage": "unknown",
                },
                execution_file="unreadable",
            )
        )
        return 0

    print(format_summary(diagnostic(payload), execution_file="present"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
