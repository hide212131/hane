#!/usr/bin/env python3
"""Verify the actual Claude Code model from its bounded SDK execution log."""
from __future__ import annotations

import json
from pathlib import Path
import re
import stat
import sys
from typing import Any

EXPECTED_MODEL = "claude-sonnet-5-5"
MODEL_ID = re.compile(r"claude-sonnet-5-5(?:-[0-9]{8})?")
USAGE_MODEL_ID = re.compile(r"[a-z0-9][a-z0-9._:-]{0,127}")
MAX_EXECUTION_BYTES = 16 * 1024 * 1024
USAGE_TOKEN_FIELDS = (
    "inputTokens",
    "outputTokens",
    "cacheReadInputTokens",
    "cacheCreationInputTokens",
)


def _is_expected_model(value: Any) -> bool:
    return isinstance(value, str) and MODEL_ID.fullmatch(value) is not None


def _has_positive_token_usage(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    return any(
        type(value.get(field)) is int and value[field] > 0
        for field in USAGE_TOKEN_FIELDS
    )



def _log_execution_file_diagnostics(path: Path | None) -> None:
    if path is None:
        diagnostics = {
            "argument_present": False,
            "exists": False,
            "file_type": "unavailable",
            "is_symlink": None,
            "is_regular_file": None,
            "lstat_size_bytes": None,
            "metadata_available": False,
        }
    else:
        try:
            metadata = path.lstat()
        except FileNotFoundError:
            diagnostics = {
                "argument_present": True,
                "exists": False,
                "file_type": "missing",
                "is_symlink": False,
                "is_regular_file": False,
                "lstat_size_bytes": None,
                "metadata_available": True,
            }
        except OSError:
            diagnostics = {
                "argument_present": True,
                "exists": None,
                "file_type": "unavailable",
                "is_symlink": None,
                "is_regular_file": None,
                "lstat_size_bytes": None,
                "metadata_available": False,
            }
        else:
            is_symlink = stat.S_ISLNK(metadata.st_mode)
            is_regular_file = stat.S_ISREG(metadata.st_mode)
            if is_symlink:
                file_type = "symlink"
            elif is_regular_file:
                file_type = "regular_file"
            elif stat.S_ISDIR(metadata.st_mode):
                file_type = "directory"
            else:
                file_type = "other"
            diagnostics = {
                "argument_present": True,
                "exists": True,
                "file_type": file_type,
                "is_symlink": is_symlink,
                "is_regular_file": is_regular_file,
                "lstat_size_bytes": metadata.st_size,
                "metadata_available": True,
            }

    print(
        "Claude execution file diagnostics: "
        + json.dumps(diagnostics, sort_keys=True, separators=(",", ":")),
        file=sys.stderr,
        flush=True,
    )


def _model_usage_diagnostics(payload: Any) -> dict[str, Any]:
    if not isinstance(payload, list):
        return {
            "event_list": False,
            "result_event_count": 0,
            "model_usage_key_present": False,
            "model_usage_value_types": [],
            "model_usage_object_count": 0,
            "model_usage_model_count": 0,
            "expected_model_entry_present": False,
            "expected_model_positive_usage": False,
        }

    result_events = [
        event
        for event in payload
        if isinstance(event, dict) and event.get("type") == "result"
    ]
    model_usage_values = [
        event["modelUsage"] for event in result_events if "modelUsage" in event
    ]
    model_usage_objects = [
        value for value in model_usage_values if isinstance(value, dict)
    ]
    model_names = [
        model
        for usage_by_model in model_usage_objects
        for model in usage_by_model
        if isinstance(model, str)
    ]
    expected_model_entry_present = any(_is_expected_model(model) for model in model_names)
    expected_model_positive_usage = any(
        _is_expected_model(model) and _has_positive_token_usage(usage)
        for usage_by_model in model_usage_objects
        for model, usage in usage_by_model.items()
    )
    return {
        "event_list": True,
        "result_event_count": len(result_events),
        "model_usage_key_present": bool(model_usage_values),
        "model_usage_value_types": sorted(
            {
                "null" if value is None else
                "object" if isinstance(value, dict) else
                "array" if isinstance(value, list) else
                "string" if isinstance(value, str) else
                "boolean" if isinstance(value, bool) else
                "number" if isinstance(value, (int, float)) else
                "other"
                for value in model_usage_values
            }
        ),
        "model_usage_object_count": len(model_usage_objects),
        "model_usage_model_count": sum(len(value) for value in model_usage_objects),
        "expected_model_entry_present": expected_model_entry_present,
        "expected_model_positive_usage": expected_model_positive_usage,
    }


def _log_model_usage_diagnostics(payload: Any) -> None:
    print(
        "Claude modelUsage diagnostics: "
        + json.dumps(
            _model_usage_diagnostics(payload),
            sort_keys=True,
            separators=(",", ":"),
        ),
        file=sys.stderr,
        flush=True,
    )

def verify_execution(payload: Any) -> dict[str, Any]:
    if not isinstance(payload, list):
        raise ValueError("Claude execution log must be a JSON event list")

    primary_models = [
        event.get("model")
        for event in payload
        if isinstance(event, dict)
        and event.get("type") == "system"
        and event.get("subtype") == "init"
        and event.get("parent_tool_use_id") is None
    ]
    if len(primary_models) != 1 or not _is_expected_model(primary_models[0]):
        raise ValueError("Claude session did not report the configured Sonnet 5.5 model")

    usage_models = set()
    has_expected_model_usage = False
    for event in payload:
        if not isinstance(event, dict) or event.get("type") != "result":
            continue
        model_usage = event.get("modelUsage")
        if not isinstance(model_usage, dict):
            continue
        for model, usage in model_usage.items():
            if not isinstance(model, str) or not USAGE_MODEL_ID.fullmatch(model):
                continue
            usage_models.add(model)
            if _is_expected_model(model) and _has_positive_token_usage(usage):
                has_expected_model_usage = True
    if not has_expected_model_usage:
        raise ValueError("Claude result did not report positive Sonnet 5.5 token usage")

    return {
        "requested_model": EXPECTED_MODEL,
        "session_model": primary_models[0],
        "usage_models": sorted(usage_models),
    }


def read_execution(path: Path | None) -> dict[str, Any]:
    _log_execution_file_diagnostics(path)
    if path is None:
        raise ValueError("Claude execution log path is missing")
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_EXECUTION_BYTES:
        raise ValueError("Claude execution log must be a bounded regular file")
    payload = json.loads(path.read_text(encoding="utf-8"))
    _log_model_usage_diagnostics(payload)
    return verify_execution(payload)


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: verify_claude_model.py EXECUTION_FILE", file=sys.stderr)
        return 2
    try:
        execution_path = Path(sys.argv[1]) if sys.argv[1] else None
        result = read_execution(execution_path)
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1

    print("### Claude model verification")
    print(f"- Requested model: `{result['requested_model']}`")
    print(f"- Session model: `{result['session_model']}`")
    print(f"- Models reporting usage: `{', '.join(result['usage_models'])}`")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
