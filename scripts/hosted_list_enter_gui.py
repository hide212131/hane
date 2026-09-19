#!/usr/bin/env python3
"""Focused macOS GUI validation for Issue #220 list-item Enter editing.

The launcher, window discovery, screenshots, fixture-byte polling, and OS-level
input helper are the same trusted pieces used by hosted_normal_list_gui.py.
Each case opens a fresh real Hane process, moves to the known fixture source
offset using synthesized System Events keystrokes, and verifies the saved
source bytes independently of the screenshots. OCR is intentionally not used
to decide the canonical caret offset.
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
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "hosted-list-enter/2"
VERIFICATION_KIND = "list_enter_editing_focused"
EXIT_PASS = 0
EXIT_NONPASS = 1
ASCII_INPUT_SOURCE = "com.apple.keylayout.ABC"


@dataclass(frozen=True)
class Case:
    name: str
    fixture: str
    click_pattern: str
    actions: tuple[tuple[str, ...], ...]
    expected: str
    purpose: str


CASES = (
    Case(
        "top_level_sibling",
        "- abc",
        "abc",
        (("press-key", "enter", "nosave"), ("type-save", "- def")),
        "- abc\n- def",
        "top-level item から marker 位置へ移動して sibling を入力",
    ),
    Case(
        "document_middle",
        "before\n\n- abc\n\nafter",
        "abc",
        (("press-key", "enter", "nosave"), ("type-save", "- def")),
        "before\n\n- abc\n- def\n\nafter",
        "文書途中の list item でも前後の source を保持",
    ),
    Case(
        "nested_sibling",
        "- abc\n  - xyz",
        "xyz",
        (("press-key", "enter", "nosave"), ("type-save", "- def")),
        "- abc\n  - xyz\n  - def",
        "nested item と同じ indentation から sibling を入力",
    ),
    Case(
        "child",
        "- abc",
        "abc",
        (("press-key", "enter", "nosave"), ("type-save", "  - xyz")),
        "- abc\n  - xyz",
        "Enter 後に追加 indentation と marker を入力して child を作成",
    ),
    Case(
        "ordinary_lazy_continuation",
        "- abc",
        "abc",
        (("press-key", "enter", "nosave"), ("type-save", "next")),
        "- abc\nnext",
        "marker ではない普通の文字を CommonMark の lazy continuation として入力",
    ),
    Case(
        "shift_enter_body",
        "- abc\n  - xyz",
        "xyz",
        (("press-shift-enter", "nosave"), ("type-save", "more")),
        "- abc\n  - xyz\n  more",
        "Shift+Enter は nested item の本文継続位置から入力",
    ),
    Case(
        "backspace_after_enter",
        "- abc",
        "abc",
        (("press-key", "enter", "nosave"), ("press-key", "backspace", "save")),
        "- abc",
        "空行直後の Backspace で元の item 末尾へ戻る",
    ),
    Case(
        "enter_twice",
        "- abc",
        "abc",
        (
            ("press-key", "enter", "nosave"),
            ("press-key", "enter", "nosave"),
            ("type-save", "paragraph"),
        ),
        "- abc\n\nparagraph",
        "Enter 2 回で list を抜けて通常 paragraph を入力",
    ),
    Case(
        "ordered_multi_digit",
        "10. abc",
        "abc",
        (("press-key", "enter", "nosave"), ("type-save", "- def")),
        "10. abc\n- def",
        "複数桁 ordered marker の実幅を使って次の marker を入力",
    ),
    Case(
        "nested_ordered",
        "10. abc\n    1. xyz",
        "xyz",
        (("press-key", "enter", "nosave"), ("type-save", "- def")),
        "10. abc\n    1. xyz\n    - def",
        "nested ordered / unordered の組み合わせで sibling を入力",
    ),
    Case(
        "trailing_newline_sibling",
        "- abc\n",
        "abc",
        (("press-key", "enter", "nosave"), ("type-save", "- def")),
        "- abc\n- def\n",
        "trailing newline を保持したまま marker caret から sibling を入力",
    ),
    Case(
        "trailing_newline_nested_body",
        "- abc\n  - xyz\n",
        "xyz",
        (("press-shift-enter", "nosave"), ("type-save", "more")),
        "- abc\n  - xyz\n  more\n",
        "trailing newline を保持した nested item の body caret を確認",
    ),
    Case(
        "nested_ime_commit_undo",
        "- abc\n  - xyz",
        "xyz",
        (
            ("press-key", "enter", "nosave"),
            (
                "type-romaji-at-caret-commit-save",
                "nihongo",
                "com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese",
            ),
            ("undo-save",),
        ),
        "- abc\n  - xyz\n",
        "nested item の IME commit 後に Undo 1回で空行へ戻る",
    ),
    Case(
        "nested_ime_cancel",
        "- abc\n  - xyz",
        "xyz",
        (
            ("press-key", "enter", "nosave"),
            (
                "type-romaji-at-caret-cancel-save",
                "nihongo",
                "com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese",
            ),
        ),
        "- abc\n  - xyz\n",
        "nested item の IME composition cancel 後に indentation を残さない",
    ),
)


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


def worst(steps: list[dict], priority: dict[str, int]) -> str:
    considered = [item["result"] for item in steps if item.get("result") in priority]
    return min(considered, key=lambda value: priority[value]) if considered else "blocked"


def reasons(steps: list[dict]) -> str:
    failures = [item["reason"] for item in steps if item.get("result") != "pass" and item.get("reason")]
    return "; ".join(failures) if failures else "すべての focused list Enter 操作が成功した"


def env_float(name: str, default: float) -> float:
    value = os.environ.get(name)
    if not value:
        return default
    parsed = float(value)
    if parsed <= 0 or parsed != parsed or parsed == float("inf"):
        raise ValueError(f"{name} must be positive and finite")
    return parsed


def run_case(
    gui_validate_module,
    interaction_module,
    env,
    target_dir: Path,
    helper,
    capture_helper,
    binary_path: Path,
    expected_sha: str,
    request_id: str,
    base_run_dir: Path,
    spec: Case,
    startup_timeout: float,
    window_timeout: float,
    helper_timeout: float,
    poll_timeout: float,
    priority: dict[str, int],
) -> dict:
    run_dir = base_run_dir / spec.name
    run_dir.mkdir(parents=True, exist_ok=True)
    fixture_path = run_dir / "fixture.md"
    fixture_path.write_bytes(spec.fixture.encode("utf-8"))
    process_holder: dict = {"process": None}
    steps: list[dict] = []
    config = interaction_module.make_config(
        gui_validate_module,
        workspace_dir=target_dir,
        scenario=f"list-enter-{spec.name}",
        expected_sha=expected_sha,
        request_id=request_id,
        generation=spec.name,
        run_dir=run_dir,
        fixture_path=fixture_path,
        features=["timing-probe"],
        extra_env={},
        startup_timeout=startup_timeout,
        window_timeout=window_timeout,
        capture_helper=capture_helper,
    )

    try:
        session_steps, window_id = interaction_module.open_session(
            gui_validate_module, env, config, binary_path, process_holder, "before", helper, helper_timeout
        )
        steps.extend(session_steps)
        pid = interaction_module.current_pid(process_holder)
        if pid is None or window_id is None:
            steps.append(step("operation", "blocked", "Hane の起動または window discovery が成功しなかった"))
            return {
                "name": spec.name,
                "purpose": spec.purpose,
                "steps": steps,
                "result": worst(steps, priority),
                "reason": reasons(steps),
                "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)},
            }

        before_screenshot = run_dir / "before.png"
        marker = spec.fixture.encode("utf-8").find(spec.click_pattern.encode("utf-8"))
        caret_offset = marker + len(spec.click_pattern.encode("utf-8"))
        if marker < 0:
            steps.append(step("caret_position", "blocked", "fixture 内に caret 用の本文がない"))
        else:
            move_right = False
            move_start_error = ""
            move_right_error = ""
            attempts = 0
            for attempts in range(1, 4):
                move_start, _, move_start_error = interaction_module.run_helper(
                    helper, ["move-doc-start", str(pid)], helper_timeout
                )
                move_right, _, move_right_error = interaction_module.run_helper(
                    helper, ["move-caret", str(pid), "right", str(caret_offset)], helper_timeout
                ) if move_start else (False, "", move_start_error)
                if move_right:
                    break
                time.sleep(0.4)
            steps.append(
                step(
                    "caret_position",
                    "pass" if move_right else "blocked",
                    None if move_right else (move_right_error or move_start_error),
                    attempts=attempts,
                    source_byte_offset=caret_offset,
                    source_pattern=spec.click_pattern,
                    screenshot=str(before_screenshot),
                )
            )
            if not move_right:
                return {
                    "name": spec.name,
                    "purpose": spec.purpose,
                    "steps": steps,
                    "result": worst(steps, priority),
                    "reason": reasons(steps),
                    "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)},
                }

            steps.append(
                interaction_module.capture_named(
                    gui_validate_module, env, config, window_id, run_dir, "caret"
                )
            )

            for index, action in enumerate(spec.actions, start=1):
                command = [action[0], str(pid), *action[1:]]
                ok, output, error = interaction_module.run_helper(helper, command, helper_timeout)
                steps.append(
                    step(
                        f"action_{index}",
                        "pass" if ok else "fail",
                        None if ok else error,
                        command=command,
                        output=output,
                    )
                )
                if not ok:
                    break
                if action[0] in ("press-key", "press-shift-enter") and action[-1] == "nosave":
                    steps.append(
                        interaction_module.capture_named(
                            gui_validate_module, env, config, window_id, run_dir, f"after_{index}"
                        )
                    )

            expected_bytes = spec.expected.encode("utf-8")
            matched, actual_bytes = interaction_module.wait_for_fixture_bytes(
                fixture_path, expected_bytes, poll_timeout
            )
            steps.append(
                step(
                    "saved_source_bytes",
                    "pass" if matched else "fail",
                    None if matched else "保存後の source bytes が期待値と一致しない",
                    expected_utf8=spec.expected,
                    expected_hex=expected_bytes.hex(),
                    actual_utf8=actual_bytes.decode("utf-8", errors="replace"),
                    actual_hex=actual_bytes.hex(),
                    fixture_path=str(fixture_path),
                )
            )
            steps.append(
                interaction_module.capture_named(
                    gui_validate_module, env, config, window_id, run_dir, "after"
                )
            )
    finally:
        steps.append(interaction_module.close_session(gui_validate_module, env, process_holder))

    return {
        "name": spec.name,
        "purpose": spec.purpose,
        "steps": steps,
        "result": worst(steps, priority),
        "reason": reasons(steps),
        "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)},
    }


def main() -> int:
    target_value = os.environ.get("HANE_LIST_ENTER_GUI_TARGET_DIR")
    control_value = os.environ.get("HANE_LIST_ENTER_GUI_CONTROL_DIR")
    expected_sha = os.environ.get("HANE_LIST_ENTER_GUI_EXPECTED_SHA", "")
    if not target_value or not control_value or not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        print("target dir, control dir and full expected SHA are required", file=sys.stderr)
        return 3

    target_dir = Path(target_value).resolve()
    control_dir = Path(control_value).resolve()
    run_dir = Path(
        os.environ.get("HANE_LIST_ENTER_GUI_RUN_DIR")
        or (Path(tempfile.gettempdir()) / "hane-list-enter-gui")
    ).resolve()
    request_id = os.environ.get("HANE_LIST_ENTER_GUI_REQUEST_ID") or f"list-enter-{os.getpid()}"
    startup_timeout = env_float("HANE_LIST_ENTER_GUI_STARTUP_TIMEOUT_SECS", 15.0)
    window_timeout = env_float("HANE_LIST_ENTER_GUI_WINDOW_TIMEOUT_SECS", 30.0)
    helper_timeout = env_float("HANE_LIST_ENTER_GUI_HELPER_TIMEOUT_SECS", 20.0)
    poll_timeout = env_float("HANE_LIST_ENTER_GUI_POLL_TIMEOUT_SECS", 20.0)
    run_dir.mkdir(parents=True, exist_ok=True)
    result_path = run_dir / "result.json"

    gui_validate_module = load_module(control_dir, "scripts/gui_validate.py", "list_enter_gui_validate")
    interaction_module = load_module(
        control_dir, "scripts/hosted_gui_interaction.py", "list_enter_hosted_gui_interaction"
    )
    env = gui_validate_module.RealEnvironment()
    priority = gui_validate_module.RESULT_PRIORITY
    top_steps: list[dict] = []
    case_results: list[dict] = []
    target_info: dict = {}
    build_info: dict = {}
    helper_tmp = tempfile.TemporaryDirectory(prefix="hane-list-enter-helper-")
    started_at = env.clock.now_iso()
    original_input_source: Optional[str] = None
    input_source_ready = False

    try:
        env.acquire_execution()
        preflight_config = interaction_module.make_config(
            gui_validate_module,
            workspace_dir=target_dir,
            scenario="list-enter-preflight",
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
        preflight, target_info = gui_validate_module.do_preflight(env, preflight_config)
        top_steps.append(preflight)
        binary_path = None
        helper = None
        capture_helper = None
        if preflight["result"] == "pass":
            try:
                helper = interaction_module.prepare_helper(
                    control_dir / "scripts" / "hosted_gui_interaction.swift", Path(helper_tmp.name)
                )
                top_steps.append(step("prepare_helper", "pass", sha256=helper.digest))
            except (OSError, subprocess.SubprocessError) as exc:
                top_steps.append(step("prepare_helper", "blocked", str(exc)))

            try:
                capture_helper = interaction_module.prepare_capture_helper(
                    control_dir / "scripts" / "window_capture_dlsym.swift", Path(helper_tmp.name)
                )
                top_steps.append(step("prepare_capture_helper", "pass", sha256=capture_helper.digest))
            except (OSError, subprocess.SubprocessError) as exc:
                top_steps.append(step("prepare_capture_helper", "blocked", str(exc)))

            if helper is not None:
                ok, source_id, error = interaction_module.run_helper(
                    helper, ["current-source"], helper_timeout
                )
                if ok:
                    original_input_source = source_id
                    top_steps.append(step("query_input_source", "pass", source_id=source_id))
                    ok, selected_source, error, source_attempts = interaction_module.select_input_source(
                        helper, ASCII_INPUT_SOURCE, helper_timeout
                    )
                    input_source_ready = ok and selected_source == ASCII_INPUT_SOURCE
                    top_steps.append(
                        step(
                            "select_ascii_input_source",
                            "pass" if input_source_ready else "blocked",
                            None if input_source_ready else (error or "ABC入力ソースを選択できない"),
                            source_id=ASCII_INPUT_SOURCE,
                            attempts=source_attempts,
                        )
                    )
                else:
                    top_steps.append(step("query_input_source", "blocked", error))

            if helper is not None:
                build_step, binary_path, build_info = gui_validate_module.do_build(
                    env, preflight_config, target_info["actual_sha"]
                )
                top_steps.append(build_step)

            if binary_path is not None and helper is not None and capture_helper is not None and input_source_ready:
                for spec in CASES:
                    try:
                        case_results.append(
                            run_case(
                                gui_validate_module,
                                interaction_module,
                                env,
                                target_dir,
                                helper,
                                capture_helper,
                                binary_path,
                                expected_sha,
                                request_id,
                                run_dir,
                                spec,
                                startup_timeout,
                                window_timeout,
                                helper_timeout,
                                poll_timeout,
                                priority,
                            )
                        )
                    except (gui_validate_module.Aborted, Exception) as exc:
                        case_results.append(
                            {"name": spec.name, "purpose": spec.purpose, "result": "blocked", "reason": str(exc)}
                        )
                        if isinstance(exc, gui_validate_module.Aborted):
                            break
            elif binary_path is not None and helper is not None:
                top_steps.append(step("case_setup", "blocked", "入力ソースの初期化に失敗した"))

        if helper is not None and original_input_source:
            ok, _output, error = interaction_module.run_helper(
                helper, ["select-source", original_input_source], helper_timeout
            )
            top_steps.append(
                step(
                    "restore_input_source",
                    "pass" if ok else "blocked",
                    None if ok else error,
                    source_id=original_input_source,
                )
            )

        all_steps = top_steps + case_results
        overall = worst(all_steps, priority)
        result = {
            "schema_version": SCHEMA_VERSION,
            "procedure_version": PROCEDURE_VERSION,
            "verification_kind": VERIFICATION_KIND,
            "request_id": request_id,
            "started_at": started_at,
            "finished_at": env.clock.now_iso(),
            "target": target_info,
            "control": {"sha": env.git_head(control_dir)},
            "runner": {
                "macos_version": platform.mac_ver()[0],
                "machine": platform.machine(),
            },
            "build": build_info,
            "top_level_steps": top_steps,
            "cases": case_results,
            "overall_result": overall,
            "overall_reason": reasons(all_steps),
        }
        label = {"pass": "PASS", "fail": "FAIL", "blocked": "BLOCKED"}.get(overall, "BLOCKED")
        result["summary"] = f"[{label}] {PROCEDURE_VERSION} — {result['overall_reason']}"
        result_path.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        (run_dir / "summary.md").write_text(result["summary"] + "\n", encoding="utf-8")
        print(result["summary"])
        print(f"result: {result_path}")
        return EXIT_PASS if overall == "pass" else EXIT_NONPASS
    except (gui_validate_module.Aborted, gui_validate_module.EnvError, OSError, subprocess.SubprocessError, ValueError) as exc:
        result = {
            "schema_version": SCHEMA_VERSION,
            "procedure_version": PROCEDURE_VERSION,
            "verification_kind": VERIFICATION_KIND,
            "request_id": request_id,
            "overall_result": "blocked",
            "overall_reason": str(exc),
            "top_level_steps": top_steps,
            "cases": case_results,
        }
        result_path.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"[BLOCKED] {PROCEDURE_VERSION} — {exc}")
        return EXIT_NONPASS
    finally:
        env.release_execution()
        helper_tmp.cleanup()


if __name__ == "__main__":
    sys.exit(main())
