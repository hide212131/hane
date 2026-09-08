#!/usr/bin/env python3
"""Hosted GUI interaction spike (Issue 44, follow-up to the launch probe).

Runs independent interactive smoke scenarios against the pinned target
checkout's Hane binary on a GitHub-hosted macOS runner:

  1. ascii_edit_save_undo_redo_reopen — OS-level select-all, type a known
     ASCII string, Cmd-S, undo+save, redo+save, then terminate and reopen
     the same fixture and check its content again.
  2. japanese_ime_input — select a built-in Japanese input source (if one
     is available), type known romaji via real key events so the OS IME
     performs the conversion, confirm/commit, Cmd-S, and check the saved
     content. Blocked (not failed) if no Japanese source is available.

This is still an interactive *smoke* check: it proves keyboard input, save,
undo/redo and reopen paths exist and produce the expected file bytes, plus a
best-effort screenshot per session. It does not prove rendering pixel-by-
pixel, and it does not cover all focus changes or dialogs. OS wheel scrolling is
checked independently using visible line numbers recognized by Vision OCR.

This script loads scripts/gui_validate.py from the trusted control
checkout via importlib and calls its preflight/build/launch/
window_discovery/capture/cleanup functions directly, so the exact same PID
lifecycle logic is reused rather than reimplemented.
"""

from __future__ import annotations

import importlib.util
import json
import os
import math
import re
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import replace
from pathlib import Path
from typing import Optional

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "hosted-gui-interaction/2"
VERIFICATION_KIND = "interactive_input_smoke"
SCOPE_NOTE = (
    "この結果はキーボード入力・保存・undo/redo・再オープン・日本語 IME 入力の"
    "およびOSスクロールの基本スモーク確認に限定される。全フォーカス移動・ダイアログ表示等の"
    "網羅的な GUI 検証はまだ証明されていない。"
)

EXIT_PASS = 0
EXIT_NONPASS = 1

ASCII_FIXTURE_ORIGINAL = "# hosted gui interaction spike\n\noriginal content\n"
ASCII_KNOWN_TEXT = "hane hosted gui ascii check"
ASCII_AFTER_APPEND = ASCII_KNOWN_TEXT + "x"
IME_FIXTURE_ORIGINAL = "# hosted gui interaction spike (ime)\n\n"
IME_ROMAJI = "nihongo"
# Expected result of typing "nihongo" then space (convert) then return
# (commit) with a stock Apple Japanese Romaji input source. This is the
# first-candidate conversion for this common word; it is recorded as an
# expectation, not guaranteed, since it depends on the runner's IME state.
IME_EXPECTED_TEXT = "日本語"


def load_pinned_gui_validate(control_dir: Path):
    sys.dont_write_bytecode = True
    module_path = control_dir / "scripts" / "gui_validate.py"
    if not module_path.is_file():
        raise SystemExit(f"pinned gui_validate.py not found at {module_path}")
    spec = importlib.util.spec_from_file_location("pinned_gui_validate", module_path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"could not load module spec for {module_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules["pinned_gui_validate"] = module
    spec.loader.exec_module(module)
    return module


def make_step(name: str, result: str, reason: Optional[str] = None, **detail) -> dict:
    return {"name": name, "result": result, "reason": reason, **detail}


def skipped_step(name: str, reason: str) -> dict:
    return {"name": name, "result": "skipped", "reason": reason}


def worst_result(steps: list[dict], priority: dict[str, int]) -> str:
    considered = [s["result"] for s in steps if s["result"] in priority]
    return min(considered, key=lambda r: priority[r]) if considered else "blocked"


def reason_for(steps: list[dict]) -> str:
    reasons = [s["reason"] for s in steps if s.get("reason") and s["result"] != "pass"]
    return "; ".join(reasons) if reasons else "すべての工程が成功した"


def run_helper(swift_helper: Path, args: list[str], timeout: float) -> tuple[bool, str, str]:
    try:
        proc = subprocess.run(
            ["swift", str(swift_helper), *args],
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return False, "", f"swift helper timed out after {timeout}s: {' '.join(args)}"
    except OSError as exc:
        return False, "", f"swift helper failed to start: {exc}"
    if proc.returncode != 0:
        return False, proc.stdout, proc.stderr.strip() or f"swift helper exited {proc.returncode}"
    return True, proc.stdout.strip(), ""


def wait_for_fixture_bytes(
    fixture_path: Path, expected: bytes, timeout: float, interval: float = 0.2
) -> tuple[bool, bytes]:
    deadline = time.monotonic() + timeout
    last = b""
    while True:
        try:
            last = fixture_path.read_bytes()
        except OSError:
            last = b""
        if last == expected:
            return True, last
        if time.monotonic() >= deadline:
            return False, last
        time.sleep(interval)


def make_config(module, *, workspace_dir, scenario, expected_sha, request_id, generation, run_dir,
                 fixture_path, features, extra_env, startup_timeout, window_timeout):
    state_dir = run_dir / "state"
    run_dir.mkdir(parents=True, exist_ok=True)
    state_dir.mkdir(parents=True, exist_ok=True)
    return module.Config(
        workspace_dir=workspace_dir,
        scenario=scenario,
        expected_sha=expected_sha,
        request_id=request_id,
        generation=generation,
        run_dir=run_dir,
        state_dir=state_dir,
        log_path=run_dir / "hane.log",
        image_path=run_dir / f"{scenario}.png",
        fixture_path=fixture_path,
        features=features,
        extra_env=extra_env,
        startup_timeout_seconds=startup_timeout,
        window_timeout_seconds=window_timeout,
    )


def capture_named(module, env, config, window_id: str, run_dir: Path, label: str) -> dict:
    capture_cfg = replace(config, image_path=run_dir / f"{label}.png")
    capture_step = module.do_capture(env, capture_cfg, window_id)
    return {**capture_step, "name": f"capture_{label}"}


def open_session(module, env, config, binary_path, process_holder, capture_label: str) -> tuple[list[dict], Optional[str]]:
    steps = []
    launch_step = module.do_launch(env, config, binary_path, process_holder)
    steps.append(launch_step)
    if launch_step["result"] != "pass":
        steps.append(skipped_step("window_discovery", "launch が pass しなかった"))
        steps.append(skipped_step(f"capture_{capture_label}", "launch が pass しなかった"))
        return steps, None

    window_step, window_id = module.do_window_discovery(env, config, process_holder["process"])
    steps.append(window_step)
    if window_step["result"] != "pass":
        steps.append(skipped_step(f"capture_{capture_label}", "window_discovery が pass しなかった"))
        return steps, None

    steps.append(capture_named(module, env, config, window_id, config.run_dir, capture_label))
    return steps, window_id


def close_session(module, env, process_holder) -> dict:
    return module.do_cleanup(env, process_holder["process"])


def current_pid(process_holder) -> Optional[int]:
    process = process_holder.get("process")
    return process.pid if process is not None else None


def verify_visible_text(helper, image_path, expected, timeout):
    ok, text, error = run_helper(helper, ["ocr", str(image_path)], timeout)
    if not ok:
        return make_step("visible_saved_text", "blocked", reason=error)
    normalized = re.sub(r"\s+", "", text).casefold()
    matched = re.sub(r"\s+", "", expected).casefold() in normalized
    return make_step("visible_saved_text", "pass" if matched else "fail",
                     reason=None if matched else "保存した文書の表示をOCRで確認できない",
                     expected=expected, recognized_text=text)


def run_ascii_scenario(module, env, target_dir, swift_helper, base_run_dir, binary_path,
                        expected_sha, request_id, startup_timeout, window_timeout,
                        helper_timeout, poll_timeout, priority) -> dict:
    run_dir = base_run_dir / "ascii_edit_save_undo_redo_reopen"
    fixture_path = run_dir / "ascii-fixture.md"
    run_dir.mkdir(parents=True, exist_ok=True)
    fixture_path.write_text(ASCII_FIXTURE_ORIGINAL, encoding="utf-8")

    steps: list[dict] = []
    process_holder: dict = {"process": None}
    config = make_config(
        module, workspace_dir=target_dir, scenario="ascii-edit-save-undo-redo",
        expected_sha=expected_sha, request_id=request_id, generation="1", run_dir=run_dir,
        fixture_path=fixture_path, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
    )

    try:
        session_steps, window_id = open_session(module, env, config, binary_path, process_holder, "before")
        steps += session_steps
        pid = current_pid(process_holder)
        if pid is not None and window_id is not None:
            ok, _out, err = run_helper(swift_helper, ["select-all-type-save", str(pid), ASCII_KNOWN_TEXT], helper_timeout)
            if not ok:
                steps.append(make_step("edit_save", "blocked", reason=err))
            else:
                matched, actual = wait_for_fixture_bytes(fixture_path, ASCII_KNOWN_TEXT.encode("utf-8"), poll_timeout)
                if matched:
                    steps.append(make_step("edit_save", "pass"))
                else:
                    steps.append(make_step(
                        "edit_save", "fail",
                        reason="保存後のフィクスチャ内容が期待した ASCII 文字列と一致しない",
                        actual=actual.decode("utf-8", errors="replace"),
                    ))

            # Selection replacement and subsequent typing are different edit
            # groups in Hane. Test undo with one deliberately separate insertion
            # instead of assuming the entire select-all/typing sequence is one.
            ok, _out, err = run_helper(swift_helper, ["append-save", str(pid), "x"], helper_timeout)
            if not ok:
                steps.append(make_step("append_save", "blocked", reason=err))
            else:
                matched, actual = wait_for_fixture_bytes(fixture_path, ASCII_AFTER_APPEND.encode(), poll_timeout)
                steps.append(make_step("append_save", "pass" if matched else "fail",
                                       reason=None if matched else "追加入力が保存されていない",
                                       actual=actual.decode("utf-8", errors="replace")))

            ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
            if not ok:
                steps.append(make_step("undo_save", "blocked", reason=err))
            else:
                matched, actual = wait_for_fixture_bytes(fixture_path, ASCII_KNOWN_TEXT.encode("utf-8"), poll_timeout)
                if matched:
                    steps.append(make_step("undo_save", "pass"))
                else:
                    steps.append(make_step(
                        "undo_save", "fail",
                        reason="undo 後に保存された内容が追加入力前の文書と一致しない",
                        actual=actual.decode("utf-8", errors="replace"),
                    ))

            ok, _out, err = run_helper(swift_helper, ["redo-save", str(pid)], helper_timeout)
            if not ok:
                steps.append(make_step("redo_save", "blocked", reason=err))
            else:
                matched, actual = wait_for_fixture_bytes(fixture_path, ASCII_AFTER_APPEND.encode("utf-8"), poll_timeout)
                if matched:
                    steps.append(make_step("redo_save", "pass"))
                else:
                    steps.append(make_step(
                        "redo_save", "fail",
                        reason="redo 後に保存された内容が期待した ASCII 文字列と一致しない",
                        actual=actual.decode("utf-8", errors="replace"),
                    ))

            if window_id is not None:
                steps.append(capture_named(module, env, config, window_id, run_dir, "after"))
        else:
            for name in ("edit_save", "undo_save", "redo_save"):
                steps.append(skipped_step(name, "対象プロセスの PID を取得できなかった"))
    finally:
        steps.append(close_session(module, env, process_holder))

    pre_reopen_result = worst_result(steps, priority)
    if pre_reopen_result != "pass":
        steps.append(skipped_step("reopen", "再オープン前の工程が pass しなかった"))
        return {"name": "ascii_edit_save_undo_redo_reopen", "steps": steps,
                "result": worst_result(steps, priority), "reason": reason_for(steps),
                "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}

    reopen_dir = run_dir / "reopen"
    reopen_process_holder: dict = {"process": None}
    reopen_config = make_config(
        module, workspace_dir=target_dir, scenario="ascii-reopen",
        expected_sha=expected_sha, request_id=request_id, generation="2", run_dir=reopen_dir,
        fixture_path=fixture_path, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
    )
    try:
        session_steps, _window_id = open_session(module, env, reopen_config, binary_path, reopen_process_holder, "reopen")
        steps += session_steps
        steps.append(verify_visible_text(swift_helper, reopen_dir / "reopen.png", ASCII_AFTER_APPEND, helper_timeout))
        matched, actual = wait_for_fixture_bytes(fixture_path, ASCII_AFTER_APPEND.encode("utf-8"), 1.0)
        step = make_step(
            "reopen_content_check", "pass" if matched else "fail",
            reason=None if matched else "再オープン後もフィクスチャ内容が期待通りであることを確認できない",
            note="ファイル内容の一致は再オープンの証跡の一部に過ぎず、描画結果そのものの証明ではない",
            actual=actual.decode("utf-8", errors="replace"),
        )
        steps.append(step)
    finally:
        steps.append(close_session(module, env, reopen_process_holder))

    return {
        "name": "ascii_edit_save_undo_redo_reopen",
        "steps": steps,
        "result": worst_result(steps, priority),
        "reason": reason_for(steps),
        "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)},
    }


def run_ime_scenario(module, env, target_dir, swift_helper, base_run_dir, binary_path,
                      expected_sha, request_id, startup_timeout, window_timeout,
                      helper_timeout, poll_timeout, priority) -> dict:
    run_dir = base_run_dir / "japanese_ime_input"
    run_dir.mkdir(parents=True, exist_ok=True)
    fixture_path = run_dir / "ime-fixture.md"
    fixture_path.write_text(IME_FIXTURE_ORIGINAL, encoding="utf-8")

    steps: list[dict] = []
    original_source: Optional[str] = None
    process_holder: dict = {"process": None}

    try:
        ok, out, err = run_helper(swift_helper, ["current-source"], helper_timeout)
        if not ok:
            steps.append(make_step("query_current_source", "blocked", reason=err))
        else:
            original_source = out
            steps.append(make_step("query_current_source", "pass", source_id=original_source))

        ok, out, err = run_helper(swift_helper, ["list-sources"], helper_timeout)
        if not ok:
            steps.append(make_step("list_input_sources", "blocked", reason=err))
            return {"name": "japanese_ime_input", "steps": steps, "result": worst_result(steps, priority),
                    "reason": reason_for(steps), "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}
        available = [line for line in out.splitlines() if line.strip()]
        steps.append(make_step("list_input_sources", "pass", available_sources=available))

        japanese_source = next(
            (source_id for source_id in available if source_id == "com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese"),
            None,
        )
        if japanese_source is None:
            steps.append(make_step(
                "select_japanese_source", "blocked",
                reason="このランナーに組み込みの日本語入力ソースが見つからない",
            ))
            steps.append(skipped_step("ime_input_save", "日本語入力ソースを選択できなかった"))
            return {"name": "japanese_ime_input", "steps": steps, "result": worst_result(steps, priority),
                    "reason": reason_for(steps),
                    "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}

        ok, _out, err = run_helper(swift_helper, ["select-source", japanese_source], helper_timeout)
        if not ok:
            steps.append(make_step("select_japanese_source", "blocked", reason=err))
            steps.append(skipped_step("ime_input_save", "日本語入力ソースを選択できなかった"))
            return {"name": "japanese_ime_input", "steps": steps, "result": worst_result(steps, priority),
                    "reason": reason_for(steps),
                    "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}
        steps.append(make_step("select_japanese_source", "pass", selected_source=japanese_source))

        config = make_config(
            module, workspace_dir=target_dir, scenario="japanese-ime-input",
            expected_sha=expected_sha, request_id=request_id, generation="1", run_dir=run_dir,
            fixture_path=fixture_path, features=["timing-probe"], extra_env={},
            startup_timeout=startup_timeout, window_timeout=window_timeout,
        )
        session_steps, window_id = open_session(module, env, config, binary_path, process_holder, "before")
        steps += session_steps
        pid = current_pid(process_holder)
        if pid is not None and window_id is not None:
            ok, _out, err = run_helper(
                swift_helper, ["type-romaji-commit-save", str(pid), IME_ROMAJI, japanese_source], helper_timeout
            )
            if not ok:
                steps.append(make_step("ime_input_save", "blocked", reason=err))
            else:
                matched, actual = wait_for_fixture_bytes(fixture_path, IME_EXPECTED_TEXT.encode("utf-8"), poll_timeout)
                steps.append(make_step(
                    "ime_input_save", "pass" if matched else "fail",
                    reason=None if matched else "保存された内容が期待した日本語文字列と一致しない",
                    romaji_input=IME_ROMAJI,
                    expected_text=IME_EXPECTED_TEXT,
                    actual_text=actual.decode("utf-8", errors="replace"),
                ))
            if window_id is not None:
                steps.append(capture_named(module, env, config, window_id, run_dir, "after"))
        else:
            steps.append(skipped_step("ime_input_save", "対象プロセスの PID を取得できなかった"))
    finally:
        steps.append(close_session(module, env, process_holder))
        if original_source:
            ok, _out, err = run_helper(swift_helper, ["select-source", original_source], helper_timeout)
            steps.append(make_step(
                "restore_input_source", "pass" if ok else "blocked",
                reason=None if ok else err, restored_source=original_source,
            ))

    return {
        "name": "japanese_ime_input",
        "steps": steps,
        "result": worst_result(steps, priority),
        "reason": reason_for(steps),
        "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)},
    }


def run_scroll_scenario(module, env, target_dir, swift_helper, base_run_dir, binary_path,
                        expected_sha, request_id, startup_timeout, window_timeout,
                        helper_timeout, poll_timeout, priority):
    run_dir = base_run_dir / "os_scroll"
    run_dir.mkdir(parents=True, exist_ok=True)
    fixture = run_dir / "scroll-fixture.md"
    contents = "\n\n".join(f"LINE {n}" for n in range(1, 101)) + "\n"
    fixture.write_text(contents, encoding="utf-8")
    config = make_config(module, workspace_dir=target_dir, scenario="os-scroll",
                         expected_sha=expected_sha, request_id=request_id, generation="1",
                         run_dir=run_dir, fixture_path=fixture, features=["timing-probe"],
                         extra_env={}, startup_timeout=startup_timeout, window_timeout=window_timeout)
    holder = {"process": None}
    steps = []
    try:
        initial, window_id = open_session(module, env, config, binary_path, holder, "before")
        steps.extend(initial)
        if window_id is not None:
            before_ok, before_text, before_error = run_helper(swift_helper, ["ocr", str(run_dir / "before.png")], helper_timeout)
            ok, detail, error = run_helper(swift_helper, ["wheel", str(current_pid(holder)), "-600"], helper_timeout)
            steps.append(make_step("os_wheel", "pass" if ok else "blocked", reason=error or None, event=detail))
            steps.append(capture_named(module, env, config, window_id, run_dir, "after"))
            after_ok, after_text, after_error = run_helper(swift_helper, ["ocr", str(run_dir / "after.png")], helper_timeout)
            if not before_ok or not after_ok:
                steps.append(make_step("visible_scroll", "blocked", reason=before_error or after_error))
            else:
                before = [int(n) for n in re.findall(r"LINE\s+(\d+)", before_text, re.I)]
                after = [int(n) for n in re.findall(r"LINE\s+(\d+)", after_text, re.I)]
                moved = bool(before and after and 1 in before and min(after) > 1 and max(after) > max(before))
                steps.append(make_step("visible_scroll", "pass" if moved else "fail",
                                       reason=None if moved else "OSスクロール後に表示行の移動を確認できない",
                                       before_lines=before, after_lines=after,
                                       before_text=before_text, after_text=after_text))
            unchanged = fixture.read_text(encoding="utf-8") == contents
            steps.append(make_step("scroll_preserves_document", "pass" if unchanged else "fail",
                                   reason=None if unchanged else "スクロールで文書内容が変わった"))
    finally:
        steps.append(close_session(module, env, holder))
    return {"name": "os_scroll", "steps": steps, "result": worst_result(steps, priority),
            "reason": reason_for(steps), "evidence": {"fixture_path": str(fixture)}}


def env_float(name: str, default: float) -> float:
    value = os.environ.get(name)
    if not value:
        return default
    parsed = float(value)
    if not math.isfinite(parsed) or parsed <= 0:
        raise ValueError(f"{name} must be positive and finite")
    return parsed


def main() -> int:
    target_dir_value = os.environ.get("HANE_GUI_INTERACTION_TARGET_DIR")
    if not target_dir_value:
        print("HANE_GUI_INTERACTION_TARGET_DIR is required", file=sys.stderr)
        return 3
    target_dir = Path(target_dir_value).resolve()

    control_dir_value = os.environ.get("HANE_GUI_INTERACTION_CONTROL_DIR")
    control_dir = Path(control_dir_value).resolve() if control_dir_value else Path(__file__).resolve().parent.parent

    swift_helper_value = os.environ.get("HANE_GUI_INTERACTION_SWIFT_HELPER")
    swift_helper = Path(swift_helper_value).resolve() if swift_helper_value else control_dir / "scripts" / "hosted_gui_interaction.swift"
    if not swift_helper.is_file():
        print(f"swift input helper not found at {swift_helper}", file=sys.stderr)
        return 3

    base_run_dir = Path(
        os.environ.get("HANE_GUI_INTERACTION_RUN_DIR")
        or (Path(tempfile.gettempdir()) / "hane-gui-interaction")
    )
    base_run_dir.mkdir(parents=True, exist_ok=True)

    request_id = os.environ.get("HANE_GUI_INTERACTION_REQUEST_ID") or (
        time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-{os.getpid()}"
    )
    expected_sha = os.environ.get("HANE_GUI_INTERACTION_EXPECTED_SHA", "")
    if not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        raise ValueError("full expected target SHA is required")
    startup_timeout = env_float("HANE_GUI_INTERACTION_STARTUP_TIMEOUT_SECS", 15.0)
    window_timeout = env_float("HANE_GUI_INTERACTION_WINDOW_TIMEOUT_SECS", 30.0)
    helper_timeout = env_float("HANE_GUI_INTERACTION_HELPER_TIMEOUT_SECS", 20.0)
    poll_timeout = env_float("HANE_GUI_INTERACTION_POLL_TIMEOUT_SECS", 10.0)

    os.chdir(target_dir)  # honor the target checkout's rust-toolchain.toml
    module = load_pinned_gui_validate(control_dir)
    env = module.RealEnvironment()
    priority = module.RESULT_PRIORITY

    lock_error = None
    try:
        env.acquire_execution()
    except module.EnvError as exc:
        lock_error = str(exc)
    try:
        started_at = env.clock.now_iso()

        preflight_config = make_config(
            module, workspace_dir=target_dir, scenario="hosted-gui-interaction-preflight",
            expected_sha=expected_sha, request_id=request_id, generation="0", run_dir=base_run_dir / "_preflight",
            fixture_path=None, features=["timing-probe"], extra_env={},
            startup_timeout=startup_timeout, window_timeout=window_timeout,
        )

        top_steps: list[dict] = []
        scenarios: list[dict] = []
        build_info: dict = {}
        target_info: dict = {}

        if lock_error:
            preflight_step = make_step("preflight", "blocked", reason=lock_error)
        else:
            preflight_step, target_info = module.do_preflight(env, preflight_config)
        top_steps.append(preflight_step)

        control_sha = "unknown"
        try:
            control_sha = env.git_head(control_dir)
        except module.EnvError as exc:
            top_steps.append(make_step("control_git_head", "blocked", reason=str(exc)))

        if preflight_step["result"] != "pass":
            top_steps.append(skipped_step("build", "preflight が pass しなかった"))
        else:
            build_step, binary_path, build_info = module.do_build(env, preflight_config)
            top_steps.append(build_step)
            if build_step["result"] != "pass":
                binary_path = None
            if binary_path is not None:
                def handle_signal(signum, _frame):
                    raise module.Aborted(f"signal {signum}")

                previous = {sig: signal.signal(sig, handle_signal)
                            for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT)}
                try:
                    for scenario_name, run_scenario in (
                        ("ascii_edit_save_undo_redo_reopen", run_ascii_scenario),
                        ("japanese_ime_input", run_ime_scenario),
                        ("os_scroll", run_scroll_scenario),
                    ):
                        try:
                            scenarios.append(run_scenario(
                                module, env, target_dir, swift_helper, base_run_dir, binary_path,
                                expected_sha, request_id, startup_timeout, window_timeout,
                                helper_timeout, poll_timeout, priority,
                            ))
                        except (module.Aborted, Exception) as exc:
                            scenarios.append({"name": scenario_name, "result": "blocked",
                                              "reason": str(exc), "steps": []})
                            if isinstance(exc, module.Aborted):
                                break
                finally:
                    for sig, handler in previous.items():
                        signal.signal(sig, handler)

        scenario_results = [s["result"] for s in scenarios] or ["blocked"]
        all_results = [s["result"] for s in top_steps if s["result"] in priority] + [
            r for r in scenario_results if r in priority
        ]
        overall_result = min(all_results, key=lambda r: priority[r]) if all_results else "blocked"
        reasons = [s.get("reason") for s in top_steps if s.get("reason") and s["result"] != "pass"]
        reasons += [s.get("reason") for s in scenarios if s.get("reason") and s["result"] != "pass"]
        overall_reason = "; ".join(r for r in reasons if r) or "すべての工程が成功した"

        result_doc = {
            "schema_version": SCHEMA_VERSION,
            "procedure_version": PROCEDURE_VERSION,
            "verification_kind": VERIFICATION_KIND,
            "request_id": request_id,
            "run_id": os.environ.get("GITHUB_RUN_ID", ""),
            "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT", ""),
            "started_at": started_at,
            "finished_at": env.clock.now_iso(),
            "target": target_info,
            "control": {"sha": control_sha},
            "build": build_info,
            "top_level_steps": top_steps,
            "scenarios": scenarios,
            "overall_result": overall_result,
            "overall_reason": overall_reason,
            "scope_note": SCOPE_NOTE,
        }
        label = {"pass": "PASS", "fail": "FAIL", "blocked": "BLOCKED"}[overall_result]
        result_doc["summary"] = (
            f"[{label}] hosted-gui-interaction request={request_id} — {overall_reason} "
            "(対話スモーク確認。網羅的な GUI 検証ではない)"
        )

        result_path = base_run_dir / "result.json"
        result_path.write_text(json.dumps(result_doc, indent=2, ensure_ascii=False) + "\n")
        summary_path = base_run_dir / "summary.md"
        summary_path.write_text(result_doc["summary"] + "\n")

        print(result_doc["summary"])
        print(f"result: {result_path}")

        return EXIT_PASS if overall_result == "pass" else EXIT_NONPASS
    finally:
        env.release_execution()



if __name__ == "__main__":
    sys.exit(main())
