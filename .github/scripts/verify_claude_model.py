#!/usr/bin/env python3
"""Verify the actual Claude Code model from its bounded SDK execution log."""
from __future__ import annotations

import json
from pathlib import Path
import re
import sys
from typing import Any

EXPECTED_MODEL = "claude-sonnet-5-5"
MODEL_ID = re.compile(r"claude-sonnet-5-5(?:-[0-9]{8})?")
USAGE_MODEL_ID = re.compile(r"[a-z0-9][a-z0-9._:-]{0,127}")
MAX_EXECUTION_BYTES = 16 * 1024 * 1024


def _is_expected_model(value: Any) -> bool:
    return isinstance(value, str) and MODEL_ID.fullmatch(value) is not None


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

    usage_models = sorted({
        model
        for event in payload
        if isinstance(event, dict) and event.get("type") == "result"
        for model in (event.get("modelUsage") or {})
        if isinstance(model, str) and USAGE_MODEL_ID.fullmatch(model)
    })
    if not any(_is_expected_model(model) for model in usage_models):
        raise ValueError("Claude result did not report Sonnet 5.5 model usage")

    return {
        "requested_model": EXPECTED_MODEL,
        "session_model": primary_models[0],
        "usage_models": usage_models,
    }


def read_execution(path: Path) -> dict[str, Any]:
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_EXECUTION_BYTES:
        raise ValueError("Claude execution log must be a bounded regular file")
    return verify_execution(json.loads(path.read_text(encoding="utf-8")))


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: verify_claude_model.py EXECUTION_FILE", file=sys.stderr)
        return 2
    try:
        result = read_execution(Path(sys.argv[1]))
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
