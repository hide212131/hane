#!/usr/bin/env python3
"""Trusted focused GUI validation for fenced code blocks (Issue #226 / #16).

One fixture, one Hane launch, OCR presence checks, one direct edit of the
hidden opening fence, one undo/save, and focused screenshots. This procedure
does not repeat comprehensive IME, inline-syntax, list, sidebar, reopen, or
full-regression scenarios.
"""

from __future__ import annotations

import importlib.util
import json
import os
import platform
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Optional

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "hosted-code-block/1"
VERIFICATION_KIND = "code_block_focused"
SCOPE_NOTE = (
    "Issue #16 の fenced code block に限定した focused GUI evidence。"
    "1 fixture / 1 launch で inactive 表示、language label、コード本文、"
    "hidden opening fence の直接編集・保存、Undo/Save による原文復元を確認する。"
    "日本語 IME、一般 inline syntax、list、sidebar、再起動、包括的 regression は含まない。"
)
EXIT_PASS = 0
EXIT_NONPASS = 1

FIXTURE_FILENAME = "code-block-focused.md"
FIXTURE_ORIGINAL = (
    "Neutral anchor\n"
    "\n"
    "~~~rust\n"
    "let answer = 42;\n"
    "**literal markdown**\n"
    "~~~\n"
    "\n"
    "Tail anchor\n"
)
OPENING_FENCE_OFFSET = FIXTURE_ORIGINAL.index("~~~rust")
FIXTURE_EDITED = (
    FIXTURE_ORIGINAL[:OPENING_FENCE_OFFSET]
    + "~"
    + FIXTURE_ORIGINAL[OPENING_FENCE_OFFSET:]
)
OCR_REQUIRED = (
    "Neutral anchor",
    "rust",
    "let answer = 42",
    "literal markdown",
    "Tail anchor",
)
MAX_LABEL_CODE_X_DELTA = 0.015


def load_module(control_dir: Path, relative: str, name: str):
    sys.dont_writebytecode = True
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


def reason_for(steps: list[dict]) -> str:
    reasons = [
        item.get("reason")
        for item in steps
        if item.get("result") not in ("pass", "skipped") and item.get("reason")
    ]
    return "; ".join(reasons) if reasons else "Issue #16 focused code-block GUI checks passed"


def overall_result(steps: list[dict], priority: dict[str, int]) -> str:
    values = [item["result"] for item in steps if item.get("result") in priority]
    return min(values, key=lambda value: priority[value]) if values else "blocked"


def normalize_ocr(text: str) -> str:
    return re.sub(r"\s+", " ", text).casefold()


def evaluate_initial_ocr(text: str) -> dict:
    normalized = normalize_ocr(text)
    missing = [expected for expected in OCR_REQUIRED if normalize_ocr(expected) not in normalized]
    return step(
        "inactive_code_block_text",
        "pass" if not missing else "fail",
        None if not missing else "inactive code block の language label / code content を OCR で確認できない",
        required=list(OCR_REQUIRED),
        missing=missing,
        recognized_text=text,
    )


def parse_click_evidence(stdout: str) -> dict:
    try:
        payload = json.loads(stdout)
    except (ValueError, TypeError) as exc:
        raise ValueError(f"click-text evidence is not valid JSON: {stdout!r}") from exc
    required = {"matched_text", "bounding_box", "window_bounds", "click_point", "edge"}
    missing = required - payload.keys()
    if missing:
        raise ValueError(f"click-text evidence is missing fields: {sorted(missing)}")
    return payload


def evaluate_fence_alignment(label_evidence: dict, code_evidence: dict) -> dict:
    label_x = float(label_evidence["bounding_box"]["minX"])
    code_x = float(code_evidence["bounding_box"]["minX"])
    delta = abs(label_x - code_x)
    return step(
        "inactive_fence_alignment",
        "pass" if delta <= MAX_LABEL_CODE_X_DELTA else "fail",
        None if delta <= MAX_LABEL_CODE_X_DELTA else (
            "language label がコード本文より大きく右へずれており、"
            "raw opening fence が前置表示されている可能性がある"
        ),
        language_label_min_x=label_x,
        code_content_min_x=code_x,
        absolute_delta=delta,
        maximum_delta=MAX_LABEL_CODE_X_DELTA,
        label_evidence=label_evidence,
        code_evidence=code_evidence,
    )


def env_float(name: str, default: float) -> float:
    value = os.environ.get(name)
    if not value:
        return default
    parsed = float(value)
    if parsed != parsed or parsed <= 0 or parsed == float("inf"):
        raise ValueError(f"{name} must be positive and finite")
    return parsed


def run_focused_scenario(
    gui_validate, interaction, env, target_dir: Path, helper, run_dir: Path,
    binary_path: Path, expected_sha: str, request_id: str,
    startup_timeout: float, window_timeout: float, helper_timeout: float,
    poll_timeout: float, priority: dict[str, int],
) -> dict:
    scenario_dir = run_dir / "fenced_code_block"
    scenario_dir.mkdir(parents=True, exist_ok=True)
    fixture = scenario_dir / FIXTURE_FILENAME
    fixture.write_text(FIXTURE_ORIGINAL, encoding="utf-8")
    config = interaction.make_config(
        gui_validate,
        workspace_dir=target_dir,
        scenario="code-block-focused",
        expected_sha=expected_sha,
        request_id=request_id,
        generation="1",
        run_dir=scenario_dir,
        fixture_path=fixture,
        features=["timing-probe"],
        extra_env={},
        startup_timeout=startup_timeout,
        window_timeout=window_timeout,
    )
    steps: list[dict] = []
    holder = {"process": None}
    try:
        session_steps, window_id = interaction.open_session(
            gui_validate, env, config, binary_path, holder, "inactive"
        )
        steps.extend(session_steps)
        pid = interaction.current_pid(holder)
        if pid is None or window_id is None:
            steps.append(skipped("inactive_ocr", "launch/window discovery が pass しなかった"))
            steps.append(skipped("direct_hidden_fence_edit", "launch/window discovery が pass しなかった"))
            steps.append(skipped("undo_restore", "launch/window discovery が pass しなかった"))
        else:
            inactive_png = scenario_dir / "inactive.png"
            ok, ocr, error = interaction.run_helper(
                helper, ["ocr", str(inactive_png)], helper_timeout
            )
            steps.append(
                evaluate_initial_ocr(ocr)
                if ok
                else step("inactive_code_block_text", "blocked", error)
            )

            alignment_evidence = []
            for pattern in (r"rust", r"let answer = 42"):
                ok_click, click_out, click_error = interaction.run_helper(
                    helper,
                    ["click-text", str(pid), str(inactive_png), pattern, "start"],
                    helper_timeout,
                )
                if not ok_click:
                    steps.append(step(
                        "inactive_fence_alignment", "blocked",
                        f"相対位置 evidence を取得できない: {click_error}",
                        pattern=pattern,
                    ))
                    alignment_evidence = []
                    break
                try:
                    alignment_evidence.append(parse_click_evidence(click_out))
                except ValueError as exc:
                    steps.append(step(
                        "inactive_fence_alignment", "blocked", str(exc), pattern=pattern
                    ))
                    alignment_evidence = []
                    break
                interaction.run_helper(
                    helper, ["move-doc-start", str(pid)], helper_timeout
                )
            if len(alignment_evidence) == 2:
                steps.append(evaluate_fence_alignment(
                    alignment_evidence[0], alignment_evidence[1]
                ))

            ok, _out, error = interaction.run_helper(
                helper, ["move-doc-start", str(pid)], helper_timeout
            )
            if not ok:
                steps.append(step("position_opening_fence", "blocked", error))
            else:
                ok, _out, error = interaction.run_helper(
                    helper,
                    ["move-caret", str(pid), "right", str(OPENING_FENCE_OFFSET)],
                    helper_timeout,
                )
                if not ok:
                    steps.append(step("position_opening_fence", "blocked", error))
                else:
                    steps.append(step(
                        "position_opening_fence", "pass",
                        source_offset=OPENING_FENCE_OFFSET,
                    ))
                    steps.append(interaction.capture_named(
                        gui_validate, env, config, window_id, scenario_dir, "fence_disclosed"
                    ))
                    ok, _out, error = interaction.run_helper(
                        helper, ["type-save", str(pid), "~"], helper_timeout
                    )
                    if not ok:
                        steps.append(step("direct_hidden_fence_edit", "blocked", error))
                    else:
                        matched, actual = interaction.wait_for_fixture_bytes(
                            fixture, FIXTURE_EDITED.encode("utf-8"), poll_timeout
                        )
                        steps.append(step(
                            "direct_hidden_fence_edit",
                            "pass" if matched else "fail",
                            None if matched else "hidden opening fence の直接編集後 bytes が期待値と一致しない",
                            expected=FIXTURE_EDITED,
                            actual=actual.decode("utf-8", errors="replace"),
                        ))
                        steps.append(interaction.capture_named(
                            gui_validate, env, config, window_id, scenario_dir, "after_fence_edit"
                        ))

                        if matched:
                            ok, _out, error = interaction.run_helper(
                                helper, ["undo-save", str(pid)], helper_timeout
                            )
                            if not ok:
                                steps.append(step("undo_restore", "blocked", error))
                            else:
                                restored, actual = interaction.wait_for_fixture_bytes(
                                    fixture, FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout
                                )
                                steps.append(step(
                                    "undo_restore",
                                    "pass" if restored else "fail",
                                    None if restored else "Undo/Save 後に元 Markdown bytes へ戻らない",
                                    expected=FIXTURE_ORIGINAL,
                                    actual=actual.decode("utf-8", errors="replace"),
                                ))
                                if restored:
                                    interaction.run_helper(
                                        helper, ["move-doc-start", str(pid)], helper_timeout
                                    )
                                    steps.append(interaction.capture_named(
                                        gui_validate, env, config, window_id,
                                        scenario_dir, "restored_inactive"
                                    ))
                        else:
                            steps.append(skipped(
                                "undo_restore", "direct edit が pass しなかった"
                            ))
    finally:
        if holder.get("process") is not None:
            steps.append(interaction.close_session(gui_validate, env, holder))

    result = overall_result(steps, priority)
    return {
        "name": "fenced_code_block",
        "steps": steps,
        "result": result,
        "reason": reason_for(steps),
        "evidence": {
            "fixture_path": str(fixture),
            "run_dir": str(scenario_dir),
            "opening_fence_source_offset": OPENING_FENCE_OFFSET,
        },
    }


def main() -> int:
    target_value = os.environ.get("HANE_CODE_BLOCK_GUI_TARGET_DIR")
    control_value = os.environ.get("HANE_CODE_BLOCK_GUI_CONTROL_DIR")
    expected_sha = os.environ.get("HANE_CODE_BLOCK_GUI_EXPECTED_SHA", "")
    if not target_value or not control_value or not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        print("target dir, control dir and full expected SHA are required", file=sys.stderr)
        return 3

    target_dir = Path(target_value).resolve()
    control_dir = Path(control_value).resolve()
    run_dir = Path(
        os.environ.get("HANE_CODE_BLOCK_GUI_RUN_DIR")
        or (Path(tempfile.gettempdir()) / "hane-code-block-gui")
    ).resolve()
    request_id = os.environ.get("HANE_CODE_BLOCK_GUI_REQUEST_ID") or f"code-block-{os.getpid()}"
    startup_timeout = env_float("HANE_CODE_BLOCK_GUI_STARTUP_TIMEOUT_SECS", 15.0)
    window_timeout = env_float("HANE_CODE_BLOCK_GUI_WINDOW_TIMEOUT_SECS", 30.0)
    helper_timeout = env_float("HANE_CODE_BLOCK_GUI_HELPER_TIMEOUT_SECS", 20.0)
    poll_timeout = env_float("HANE_CODE_BLOCK_GUI_POLL_TIMEOUT_SECS", 20.0)
    run_dir.mkdir(parents=True, exist_ok=True)
    result_path = run_dir / "result.json"

    gui_validate = load_module(
        control_dir, "scripts/gui_validate.py", "code_block_gui_validate"
    )
    interaction = load_module(
        control_dir, "scripts/hosted_gui_interaction.py", "code_block_gui_interaction"
    )
    env = gui_validate.RealEnvironment()
    priority = gui_validate.RESULT_PRIORITY
    top_steps: list[dict] = []
    scenarios: list[dict] = []
    target_info: dict = {}
    build_info: dict = {}
    started_at = env.clock.now_iso()
    helper_tmp = tempfile.TemporaryDirectory(prefix="hane-code-block-helper-")

    try:
        env.acquire_execution()
        config = interaction.make_config(
            gui_validate,
            workspace_dir=target_dir,
            scenario="code-block-preflight",
            expected_sha=expected_sha,
            request_id=request_id,
            generation="0",
            run_dir=run_dir / "_preflight",
            fixture_path=None,
            features=["timing-probe"],
            extra_env={},
            startup_timeout=startup_timeout,
            window_timeout=window_timeout,
        )
        preflight, target_info = gui_validate.do_preflight(env, config)
        top_steps.append(preflight)
        binary_path = None
        helper = None
        if preflight["result"] == "pass":
            try:
                helper = interaction.prepare_helper(
                    control_dir / "scripts" / "hosted_gui_interaction.swift",
                    Path(helper_tmp.name),
                )
                top_steps.append(step("prepare_helper", "pass", sha256=helper.digest))
            except (OSError, subprocess.SubprocessError) as exc:
                top_steps.append(step("prepare_helper", "blocked", str(exc)))

            if helper is not None:
                build_step, binary_path, build_info = gui_validate.do_build(
                    env, config, target_info["actual_sha"]
                )
                top_steps.append(build_step)
                if build_step["result"] != "pass":
                    binary_path = None
        else:
            top_steps.append(skipped("prepare_helper", "preflight が pass しなかった"))
            top_steps.append(skipped("build", "preflight が pass しなかった"))

        if binary_path is not None and helper is not None:
            scenarios.append(run_focused_scenario(
                gui_validate, interaction, env, target_dir, helper, run_dir,
                binary_path, expected_sha, request_id,
                startup_timeout, window_timeout, helper_timeout, poll_timeout, priority,
            ))

        values = [s["result"] for s in top_steps if s.get("result") in priority]
        values += [s["result"] for s in scenarios if s.get("result") in priority]
        overall = min(values, key=lambda value: priority[value]) if values else "blocked"
        reasons = [
            s.get("reason") for s in top_steps
            if s.get("result") not in ("pass", "skipped") and s.get("reason")
        ]
        reasons += [
            s.get("reason") for s in scenarios
            if s.get("result") != "pass" and s.get("reason")
        ]
        reason = "; ".join(reasons) or "Issue #16 focused code-block GUI checks passed"
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
            "runner": {
                "os": os.environ.get("RUNNER_OS", ""),
                "arch": os.environ.get("RUNNER_ARCH", ""),
                "macos_version": platform.mac_ver()[0],
                "machine": platform.machine(),
            },
            "build": build_info,
            "top_level_steps": top_steps,
            "scenarios": scenarios,
            "overall_result": overall,
            "overall_reason": reason,
        }
        label = {
            "pass": "PASS",
            "fail": "FAIL",
            "blocked": "BLOCKED",
        }.get(overall, "BLOCKED")
        result["summary"] = f"[{label}] {PROCEDURE_VERSION} — {reason}"
        result_path.write_text(
            json.dumps(result, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        (run_dir / "summary.md").write_text(
            result["summary"] + "\n", encoding="utf-8"
        )
        print(result["summary"])
        return EXIT_PASS if overall == "pass" else EXIT_NONPASS
    except (
        gui_validate.Aborted,
        gui_validate.EnvError,
        OSError,
        subprocess.SubprocessError,
        ValueError,
        RuntimeError,
    ) as exc:
        fallback = {
            "schema_version": SCHEMA_VERSION,
            "procedure_version": PROCEDURE_VERSION,
            "verification_kind": VERIFICATION_KIND,
            "scope_note": SCOPE_NOTE,
            "request_id": request_id,
            "overall_result": "blocked",
            "overall_reason": str(exc),
            "top_level_steps": top_steps,
            "scenarios": scenarios,
        }
        result_path.write_text(
            json.dumps(fallback, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        print(f"[BLOCKED] {PROCEDURE_VERSION} — {exc}")
        return EXIT_NONPASS
    finally:
        env.release_execution()
        helper_tmp.cleanup()


if __name__ == "__main__":
    sys.exit(main())
