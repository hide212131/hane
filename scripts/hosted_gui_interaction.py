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
import hashlib
import json
import os
import math
import platform
import re
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Optional

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "hosted-gui-interaction/5"
VERIFICATION_KIND = "interactive_input_smoke"
SCOPE_NOTE = (
    "この結果はキーボード入力・保存・undo/redo・再オープン・日本語 IME 入力・"
    "OSスクロール、および太字/斜体複合・複数行 inline code・quote・list における"
    "marker/本文境界へのクリック・caret移動・ドラッグ選択・区切り記号の追加削除の基本スモーク確認に"
    "限定される。区切り記号の未閉鎖/再閉鎖、および既存の複数行 code span 自体の閉じ backtick の"
    "削除・再入力では、caretを中立位置へ移して撮影した描画ピクセルのdigestが変化し、"
    "再閉鎖後に初期閉鎖状態へ戻ることも確認する。OCR は座標特定にのみ使用し、"
    "bold/italic等の意味的なstyle種別そのものはOCRでは判定しない。全フォーカス移動・ダイアログ表示等の"
    "網羅的な GUI 検証はまだ証明されていない。"
)

EXIT_PASS = 0
EXIT_NONPASS = 1

ASCII_FIXTURE_ORIGINAL = "# hosted gui interaction spike\n\noriginal content\n"
ASCII_KNOWN_TEXT = "hane hosted gui ascii check"
ASCII_AFTER_APPEND = ASCII_KNOWN_TEXT + "x"
IME_FIXTURE_ORIGINAL = "# hosted gui interaction spike (ime)\n\n"
IME_ROMAJI = "nihongo"
IME_EXPECTED_TEXT = "日本語"

INLINE_FIXTURE_ORIGINAL = (
    "# hosted gui interaction inline syntax spike\n"
    "\n"
    "**bold *italic* combo** boundary line.\n"
    "\n"
    "this inline `code\n"
    "span` crosses a line.\n"
    "\n"
    "> quote with **bold** inside.\n"
    "\n"
    "- list item with *italic* inside.\n"
)

BOLD_ITALIC_OPEN_RE = r"\*\*(?=bold \*italic)"
BOLD_ITALIC_CLOSE_RE = r"(?<=combo)\*\*"
CODE_SPAN_OPEN_RE = r"`(?=code)"
CODE_SPAN_CLOSE_RE = r"(?<=span)`"
QUOTE_BOLD_OPEN_RE = r"(?<=quote with )\*\*(?=bold)"
LIST_ITALIC_OPEN_RE = r"(?<=item with )\*(?=italic)"

BOLD_ITALIC_OCR_RE = r"bold italic combo"
CODE_SPAN_OPEN_OCR_RE = r"code"
CODE_SPAN_CLOSE_OCR_RE = r"span"
QUOTE_BOLD_OPEN_OCR_RE = r"bold(?= inside)"
LIST_ITALIC_OPEN_OCR_RE = r"italic(?= inside)"
DRAG_SELECT_START_OCR_RE = r"(?<=b)old(?= italic combo)"
DRAG_SELECT_END_OCR_RE = r"com(?=bo boundary)"
BOUNDARY_MARK = "Z"
NAVIGATION_MARK = "N"
JAPANESE_SOURCE = "com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese"


def insert_at_match(text: str, pattern: str, insertion: str, *, edge: str) -> str:
    match = re.search(pattern, text)
    if not match:
        raise ValueError(f"pattern not found in expected fixture text: {pattern}")
    position = match.end() if edge == "end" else match.start()
    return text[:position] + insertion + text[position:]


def delimiter_states(delimiter: str) -> tuple[str, str]:
    if delimiter not in ("*", "**", "`"):
        raise ValueError("unsupported delimiter")
    closed = INLINE_FIXTURE_ORIGINAL + f" {delimiter}loose{delimiter} tail"
    unclosed = INLINE_FIXTURE_ORIGINAL + f" {delimiter}loose tail"
    return unclosed, closed


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


@dataclass(frozen=True)
class PreparedHelper:
    binary: Path
    digest: str


def prepare_helper(source: Path, directory: Path) -> PreparedHelper:
    binary = directory / "interaction-helper"
    subprocess.run(
        ["/usr/bin/swiftc", str(source), "-o", str(binary)],
        capture_output=True,
        text=True,
        check=True,
        timeout=120,
    )
    binary.chmod(0o500)
    return PreparedHelper(binary, hashlib.sha256(binary.read_bytes()).hexdigest())


def run_helper(swift_helper: PreparedHelper, args: list[str], timeout: float) -> tuple[bool, str, str]:
    try:
        if hashlib.sha256(swift_helper.binary.read_bytes()).hexdigest() != swift_helper.digest:
            return False, "", "compiled interaction helper integrity mismatch"
        proc = subprocess.run(
            [str(swift_helper.binary), *args], capture_output=True, text=True, timeout=timeout
        )
        if hashlib.sha256(swift_helper.binary.read_bytes()).hexdigest() != swift_helper.digest:
            return False, "", "compiled interaction helper integrity mismatch"
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
    return make_step(
        "visible_saved_text", "pass" if matched else "fail",
        reason=None if matched else "保存した文書の表示をOCRで確認できない",
        expected=expected, recognized_text=text,
    )


def image_pixel_digest(helper, image_path: Path, timeout: float) -> tuple[Optional[str], Optional[str]]:
    ok, value, error = run_helper(helper, ["image-digest", str(image_path)], timeout)
    if not ok:
        return None, error
    if not re.fullmatch(r"[0-9a-f]{64}", value):
        return None, f"invalid image pixel digest: {value!r}"
    return value, None


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
                steps.append(make_step("edit_save", "pass" if matched else "fail",
                                       reason=None if matched else "保存後のフィクスチャ内容が期待した ASCII 文字列と一致しない",
                                       actual=actual.decode("utf-8", errors="replace")))
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
                steps.append(make_step("undo_save", "pass" if matched else "fail",
                                       reason=None if matched else "undo 後に保存された内容が追加入力前の文書と一致しない",
                                       actual=actual.decode("utf-8", errors="replace")))
            ok, _out, err = run_helper(swift_helper, ["redo-save", str(pid)], helper_timeout)
            if not ok:
                steps.append(make_step("redo_save", "blocked", reason=err))
            else:
                matched, actual = wait_for_fixture_bytes(fixture_path, ASCII_AFTER_APPEND.encode("utf-8"), poll_timeout)
                steps.append(make_step("redo_save", "pass" if matched else "fail",
                                       reason=None if matched else "redo 後に保存された内容が期待した ASCII 文字列と一致しない",
                                       actual=actual.decode("utf-8", errors="replace")))
            steps.append(capture_named(module, env, config, window_id, run_dir, "after"))
        else:
            for name in ("edit_save", "append_save", "undo_save", "redo_save"):
                steps.append(skipped_step(name, "対象プロセスの PID を取得できなかった"))
    finally:
        steps.append(close_session(module, env, process_holder))

    if worst_result(steps, priority) != "pass":
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
        steps.append(make_step(
            "reopen_content_check", "pass" if matched else "fail",
            reason=None if matched else "再オープン後もフィクスチャ内容が期待通りであることを確認できない",
            note="ファイル内容の一致は再オープンの証跡の一部に過ぎず、描画結果そのものの証明ではない",
            actual=actual.decode("utf-8", errors="replace"),
        ))
    finally:
        steps.append(close_session(module, env, reopen_process_holder))
    return {"name": "ascii_edit_save_undo_redo_reopen", "steps": steps,
            "result": worst_result(steps, priority), "reason": reason_for(steps),
            "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}


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
        japanese_source = next((source_id for source_id in available if source_id == JAPANESE_SOURCE), None)
        if japanese_source is None:
            steps.append(make_step("select_japanese_source", "blocked", reason="このランナーに組み込みの日本語入力ソースが見つからない"))
            steps.append(skipped_step("ime_input_save", "日本語入力ソースを選択できなかった"))
            return {"name": "japanese_ime_input", "steps": steps, "result": worst_result(steps, priority),
                    "reason": reason_for(steps), "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}
        ok, _out, err = run_helper(swift_helper, ["select-source", japanese_source], helper_timeout)
        steps.append(make_step("select_japanese_source", "pass" if ok else "blocked", reason=None if ok else err,
                               selected_source=japanese_source))
        if not ok:
            steps.append(skipped_step("ime_input_save", "日本語入力ソースを選択できなかった"))
            return {"name": "japanese_ime_input", "steps": steps, "result": worst_result(steps, priority),
                    "reason": reason_for(steps), "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}
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
            ok, _out, err = run_helper(swift_helper, ["type-romaji-commit-save", str(pid), IME_ROMAJI, japanese_source], helper_timeout)
            if not ok:
                steps.append(make_step("ime_input_save", "blocked", reason=err))
            else:
                matched, actual = wait_for_fixture_bytes(fixture_path, IME_EXPECTED_TEXT.encode("utf-8"), poll_timeout)
                steps.append(make_step("ime_input_save", "pass" if matched else "fail",
                                       reason=None if matched else "保存された内容が期待した日本語文字列と一致しない",
                                       romaji_input=IME_ROMAJI, expected_text=IME_EXPECTED_TEXT,
                                       actual_text=actual.decode("utf-8", errors="replace")))
            steps.append(capture_named(module, env, config, window_id, run_dir, "after"))
        else:
            steps.append(skipped_step("ime_input_save", "対象プロセスの PID を取得できなかった"))
    finally:
        steps.append(close_session(module, env, process_holder))
        if original_source:
            ok, _out, err = run_helper(swift_helper, ["select-source", original_source], helper_timeout)
            steps.append(make_step("restore_input_source", "pass" if ok else "blocked",
                                   reason=None if ok else err, restored_source=original_source))
    return {"name": "japanese_ime_input", "steps": steps,
            "result": worst_result(steps, priority), "reason": reason_for(steps),
            "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}


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
                                       before_lines=before, after_lines=after, before_text=before_text, after_text=after_text))
            unchanged = fixture.read_text(encoding="utf-8") == contents
            steps.append(make_step("scroll_preserves_document", "pass" if unchanged else "fail",
                                   reason=None if unchanged else "スクロールで文書内容が変わった"))
    finally:
        steps.append(close_session(module, env, holder))
    return {"name": "os_scroll", "steps": steps, "result": worst_result(steps, priority),
            "reason": reason_for(steps), "evidence": {"fixture_path": str(fixture)}}


def _decode(data: bytes) -> str:
    return data.decode("utf-8", errors="replace")


def _move_to_neutral(swift_helper, pid: int, helper_timeout: float, name: str) -> dict:
    ok, _out, err = run_helper(swift_helper, ["move-doc-start", str(pid)], helper_timeout)
    return make_step(name, "pass" if ok else "blocked", reason=None if ok else err)


def boundary_edit_check(swift_helper, screenshot_path, pid, fixture_path, baseline,
                        ocr_pattern, ocr_edge, source_pattern, source_edge, insertion,
                        helper_timeout, poll_timeout):
    detail = {"screenshot": str(screenshot_path)}
    ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(screenshot_path), ocr_pattern, ocr_edge], helper_timeout)
    if not ok:
        return False, f"境界へのクリックに失敗した: {err}", detail
    ok, _out, err = run_helper(swift_helper, ["type-save", str(pid), insertion], helper_timeout)
    if not ok:
        return False, f"境界への入力に失敗した: {err}", detail
    expected = insert_at_match(baseline, source_pattern, insertion, edge=source_edge)
    matched, actual = wait_for_fixture_bytes(fixture_path, expected.encode("utf-8"), poll_timeout)
    detail.update(expected_after_insert=expected, actual_after_insert=_decode(actual))
    if not matched:
        return False, "境界クリック挿入後の内容が期待値と一致しない", detail
    ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
    if not ok:
        return False, f"undo に失敗した: {err}", detail
    matched, actual = wait_for_fixture_bytes(fixture_path, baseline.encode("utf-8"), poll_timeout)
    detail.update(expected_after_undo=baseline, actual_after_undo=_decode(actual))
    if not matched:
        return False, "undo 後に元の内容へ戻らない", detail
    return True, None, detail


def run_boundary_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                      name, checks, helper_timeout, poll_timeout) -> list[dict]:
    pid = current_pid(process_holder)
    if pid is None or window_id is None:
        return [skipped_step(name, "対象プロセスの PID またはウィンドウを取得できなかった")]
    result: list[dict] = []
    for index, (ocr_pattern, ocr_edge, source_pattern, source_edge) in enumerate(checks):
        label = f"{name}_{index}"
        capture = capture_named(module, env, config, window_id, run_dir, label)
        result.append(capture)
        if capture["result"] != "pass":
            result.append(make_step(name, "blocked", reason=f"境界確認用の撮影に失敗した: {capture.get('reason')}"))
            return result
        screenshot = run_dir / f"{label}.png"
        ok, reason, detail = boundary_edit_check(
            swift_helper, screenshot, pid, config.fixture_path, INLINE_FIXTURE_ORIGINAL,
            ocr_pattern, ocr_edge, source_pattern, source_edge, BOUNDARY_MARK,
            helper_timeout, poll_timeout,
        )
        result.append(make_step(f"{name}_check_{index}", "pass" if ok else "fail", reason=reason, **detail))
        if not ok:
            result.append(make_step(name, "fail", reason=reason))
            return result
        reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_reset_{index}")
        result.append(reset)
        if reset["result"] != "pass":
            result.append(make_step(name, "blocked", reason=reset.get("reason")))
            return result
    result.append(make_step(name, "pass"))
    return result


def run_boundary_navigation_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                                 helper_timeout, poll_timeout) -> list[dict]:
    name = "boundary_caret_navigation"
    pid = current_pid(process_holder)
    if pid is None or window_id is None:
        return [skipped_step(name, "対象プロセスの PID またはウィンドウを取得できなかった")]
    expected_left = INLINE_FIXTURE_ORIGINAL.replace("**bold", f"*{NAVIGATION_MARK}*bold", 1)
    expected_right = INLINE_FIXTURE_ORIGINAL.replace("**bold", f"**b{NAVIGATION_MARK}old", 1)
    expected_up = INLINE_FIXTURE_ORIGINAL.replace("this inline `code", f"this{NAVIGATION_MARK} inline `code", 1)
    cases = (
        ("left_across_open_marker", BOLD_ITALIC_OCR_RE, "start", "left", expected_left),
        ("right_into_visible_text", BOLD_ITALIC_OCR_RE, "start", "right", expected_right),
        ("up_from_multiline_code_close", CODE_SPAN_CLOSE_OCR_RE, "end", "up", expected_up),
    )
    steps: list[dict] = []
    for index, (case_name, ocr_pattern, ocr_edge, direction, expected) in enumerate(cases):
        reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_reset_before_{index}")
        steps.append(reset)
        if reset["result"] != "pass":
            steps.append(make_step(name, "blocked", reason=reset.get("reason")))
            return steps
        label = f"{name}_{index}"
        capture = capture_named(module, env, config, window_id, run_dir, label)
        steps.append(capture)
        if capture["result"] != "pass":
            steps.append(make_step(name, "blocked", reason=capture.get("reason")))
            return steps
        screenshot = run_dir / f"{label}.png"
        ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(screenshot), ocr_pattern, ocr_edge], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"caret navigation の境界クリックに失敗した: {err}"))
            return steps
        ok, _out, err = run_helper(swift_helper, ["move-caret", str(pid), direction, "1"], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"caret {direction} 移動に失敗した: {err}"))
            return steps
        ok, _out, err = run_helper(swift_helper, ["type-save", str(pid), NAVIGATION_MARK], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"caret 移動後の着地点確認入力に失敗した: {err}"))
            return steps
        matched, actual = wait_for_fixture_bytes(config.fixture_path, expected.encode("utf-8"), poll_timeout)
        detail = {
            "case": case_name,
            "screenshot": str(screenshot),
            "direction": direction,
            "count": 1,
            "expected_after_move_insert": expected,
            "actual_after_move_insert": _decode(actual),
        }
        if not matched:
            steps.append(make_step(f"{name}_check_{index}", "fail", reason="caret 移動後の source 着地点が期待値と一致しない", **detail))
            steps.append(make_step(name, "fail", reason="caret 移動後の source 着地点が期待値と一致しない"))
            return steps
        ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"caret navigation undo に失敗した: {err}"))
            return steps
        restored, actual = wait_for_fixture_bytes(config.fixture_path, INLINE_FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout)
        detail.update(expected_after_undo=INLINE_FIXTURE_ORIGINAL, actual_after_undo=_decode(actual))
        steps.append(make_step(f"{name}_check_{index}", "pass" if restored else "fail",
                               reason=None if restored else "caret navigation undo 後に元の内容へ戻らない", **detail))
        if not restored:
            steps.append(make_step(name, "fail", reason="caret navigation undo 後に元の内容へ戻らない"))
            return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_reset_after")
    steps.append(reset)
    steps.append(make_step(name, "pass" if reset["result"] == "pass" else "blocked", reason=reset.get("reason")))
    return steps


def run_boundary_ime_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                          helper_timeout, poll_timeout) -> list[dict]:
    name = "boundary_ime_input"
    pid = current_pid(process_holder)
    if pid is None or window_id is None:
        return [skipped_step(name, "対象プロセスの PID またはウィンドウを取得できなかった")]
    steps: list[dict] = []
    original_source: Optional[str] = None
    try:
        ok, out, err = run_helper(swift_helper, ["current-source"], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=err))
            return steps
        original_source = out
        ok, out, err = run_helper(swift_helper, ["list-sources"], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=err))
            return steps
        available = [line for line in out.splitlines() if line.strip()]
        if JAPANESE_SOURCE not in available:
            steps.append(make_step(name, "blocked", reason="このランナーに組み込みの日本語入力ソースが見つからない"))
            return steps
        reset = _move_to_neutral(swift_helper, pid, helper_timeout, "boundary_ime_input_reset_before")
        steps.append(reset)
        if reset["result"] != "pass":
            steps.append(make_step(name, "blocked", reason=reset.get("reason")))
            return steps
        capture = capture_named(module, env, config, window_id, run_dir, "boundary_ime_input")
        steps.append(capture)
        if capture["result"] != "pass":
            steps.append(make_step(name, "blocked", reason=capture.get("reason")))
            return steps
        screenshot = run_dir / "boundary_ime_input.png"
        ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(screenshot), BOLD_ITALIC_OCR_RE, "start"], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"IME 境界クリックに失敗した: {err}"))
            return steps
        ok, _out, err = run_helper(swift_helper, ["type-romaji-at-caret-commit-save", str(pid), IME_ROMAJI, JAPANESE_SOURCE], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"境界 IME 入力に失敗した: {err}"))
            return steps
        expected = insert_at_match(INLINE_FIXTURE_ORIGINAL, BOLD_ITALIC_OPEN_RE, IME_EXPECTED_TEXT, edge="end")
        matched, actual = wait_for_fixture_bytes(config.fixture_path, expected.encode("utf-8"), poll_timeout)
        detail = {"screenshot": str(screenshot), "expected_after_insert": expected, "actual_after_insert": _decode(actual)}
        if not matched:
            steps.append(make_step("boundary_ime_input_check", "fail", reason="境界 IME 入力後の内容が期待値と一致しない", **detail))
            steps.append(make_step(name, "fail", reason="境界 IME 入力後の内容が期待値と一致しない"))
            return steps
        ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"境界 IME undo に失敗した: {err}"))
            return steps
        matched, actual = wait_for_fixture_bytes(config.fixture_path, INLINE_FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout)
        detail.update(expected_after_undo=INLINE_FIXTURE_ORIGINAL, actual_after_undo=_decode(actual))
        steps.append(make_step("boundary_ime_input_check", "pass" if matched else "fail",
                               reason=None if matched else "境界 IME undo 後に元の内容へ戻らない", **detail))
        if not matched:
            steps.append(make_step(name, "fail", reason="境界 IME undo 後に元の内容へ戻らない"))
            return steps
        reset = _move_to_neutral(swift_helper, pid, helper_timeout, "boundary_ime_input_reset_after")
        steps.append(reset)
        steps.append(make_step(name, "pass" if reset["result"] == "pass" else "blocked", reason=reset.get("reason")))
        return steps
    finally:
        if original_source:
            ok, _out, err = run_helper(swift_helper, ["select-source", original_source], helper_timeout)
            steps.append(make_step("restore_boundary_ime_input_source", "pass" if ok else "blocked",
                                   reason=None if ok else err, restored_source=original_source))
        else:
            steps.append(make_step("restore_boundary_ime_input_source", "blocked", reason="元の入力ソースを取得できなかった"))


def run_drag_select_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                         helper_timeout, poll_timeout) -> list[dict]:
    name = "drag_select_delete_undo_redo"
    pid = current_pid(process_holder)
    if pid is None or window_id is None:
        return [skipped_step(name, "対象プロセスの PID またはウィンドウを取得できなかった")]
    steps: list[dict] = []
    capture = capture_named(module, env, config, window_id, run_dir, "drag_select_state0")
    steps.append(capture)
    if capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=capture.get("reason")))
        return steps
    screenshot = run_dir / "drag_select_state0.png"
    ok, _out, err = run_helper(swift_helper, ["drag-select-text", str(pid), str(screenshot),
                                                DRAG_SELECT_START_OCR_RE, "start", DRAG_SELECT_END_OCR_RE, "end"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"ドラッグ選択に失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["delete-selection-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"選択範囲の削除に失敗した: {err}"))
        return steps
    deleted_expected = INLINE_FIXTURE_ORIGINAL.replace("old *italic* com", "", 1)
    matched, actual = wait_for_fixture_bytes(config.fixture_path, deleted_expected.encode("utf-8"), poll_timeout)
    detail = {"screenshot": str(screenshot), "deleted_expected": deleted_expected, "deleted_actual": _decode(actual)}
    if not matched:
        steps.append(make_step("drag_select_delete_undo_redo_check", "fail", reason="ドラッグ選択範囲の削除結果が一致しない", **detail))
        steps.append(make_step(name, "fail", reason="ドラッグ選択範囲の削除結果が一致しない"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"undo に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, INLINE_FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout)
    detail["undo_actual"] = _decode(actual)
    if not matched:
        steps.append(make_step("drag_select_delete_undo_redo_check", "fail", reason="undo 後に元の内容へ戻らない", **detail))
        steps.append(make_step(name, "fail", reason="undo 後に元の内容へ戻らない"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["redo-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"redo に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, deleted_expected.encode("utf-8"), poll_timeout)
    detail["redo_actual"] = _decode(actual)
    if not matched:
        steps.append(make_step("drag_select_delete_undo_redo_check", "fail", reason="redo 後に削除結果へ戻らない", **detail))
        steps.append(make_step(name, "fail", reason="redo 後に削除結果へ戻らない"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"最終 undo に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, INLINE_FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout)
    detail["restored_actual"] = _decode(actual)
    steps.append(make_step("drag_select_delete_undo_redo_check", "pass" if matched else "fail",
                           reason=None if matched else "最終 undo 後に元の内容へ戻らない", **detail))
    if not matched:
        steps.append(make_step(name, "fail", reason="最終 undo 後に元の内容へ戻らない"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, "drag_select_reset")
    steps.append(reset)
    steps.append(make_step(name, "pass" if reset["result"] == "pass" else "blocked", reason=reset.get("reason")))
    return steps


def run_delimiter_toggle_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                              kind: str, delimiter: str, helper_timeout, poll_timeout) -> list[dict]:
    name = f"delimiter_toggle_{kind}"
    pid = current_pid(process_holder)
    if pid is None or window_id is None:
        return [skipped_step(name, "対象プロセスの PID またはウィンドウを取得できなかった")]
    steps: list[dict] = []
    unclosed_expected, closed_expected = delimiter_states(delimiter)
    ok, _out, err = run_helper(swift_helper, ["end-doc-type-save", str(pid), f" {delimiter}loose{delimiter} tail"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"区切り記号の追加に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, closed_expected.encode("utf-8"), poll_timeout)
    if not matched:
        steps.append(make_step(name, "fail", reason=f"{delimiter!r} を追加した内容が期待値と一致しない: actual={_decode(actual)!r}"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_initial_reset")
    steps.append(reset)
    if reset["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=reset.get("reason")))
        return steps
    initial_capture = capture_named(module, env, config, window_id, run_dir, f"{name}_initial_closed")
    steps.append(initial_capture)
    if initial_capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=initial_capture.get("reason")))
        return steps
    initial_image = run_dir / f"{name}_initial_closed.png"
    initial_digest, digest_error = image_pixel_digest(swift_helper, initial_image, helper_timeout)
    if digest_error:
        steps.append(make_step(name, "blocked", reason=f"初期閉鎖状態の描画 digest 取得に失敗した: {digest_error}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(initial_image), r"loose", "end"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ区切り記号の位置決めに失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["shift-select", str(pid), "right", str(len(delimiter))], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ区切り記号の選択に失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["delete-selection-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ区切り記号の削除に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, unclosed_expected.encode("utf-8"), poll_timeout)
    unclosed_actual = _decode(actual)
    if not matched:
        steps.append(make_step(name, "fail", reason=f"{delimiter!r} 削除後の未閉鎖内容が期待値と一致しない"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_unclosed_reset")
    steps.append(reset)
    if reset["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=reset.get("reason")))
        return steps
    unclosed_capture = capture_named(module, env, config, window_id, run_dir, f"{name}_unclosed")
    steps.append(unclosed_capture)
    if unclosed_capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=f"未閉鎖状態の撮影に失敗した: {unclosed_capture.get('reason')}"))
        return steps
    unclosed_image = run_dir / f"{name}_unclosed.png"
    unclosed_digest, digest_error = image_pixel_digest(swift_helper, unclosed_image, helper_timeout)
    if digest_error:
        steps.append(make_step(name, "blocked", reason=f"未閉鎖状態の描画 digest 取得に失敗した: {digest_error}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(unclosed_image), r"loose", "end"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"再閉鎖位置の位置決めに失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["type-save", str(pid), delimiter], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ区切り記号の再入力に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, closed_expected.encode("utf-8"), poll_timeout)
    closed_actual = _decode(actual)
    if not matched:
        steps.append(make_step(name, "fail", reason=f"{delimiter!r} 再入力後の内容が期待値と一致しない"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_closed_reset")
    steps.append(reset)
    if reset["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=reset.get("reason")))
        return steps
    closed_capture = capture_named(module, env, config, window_id, run_dir, f"{name}_closed")
    steps.append(closed_capture)
    if closed_capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=f"閉鎖状態の撮影に失敗した: {closed_capture.get('reason')}"))
        return steps
    closed_image = run_dir / f"{name}_closed.png"
    closed_digest, digest_error = image_pixel_digest(swift_helper, closed_image, helper_timeout)
    if digest_error:
        steps.append(make_step(name, "blocked", reason=f"再閉鎖状態の描画 digest 取得に失敗した: {digest_error}"))
        return steps
    visual_transition = unclosed_digest != closed_digest
    visual_restored = initial_digest == closed_digest
    if not visual_transition or not visual_restored:
        reason = (
            "未閉鎖/再閉鎖で描画ピクセルが変化しない"
            if not visual_transition else "再閉鎖後の描画が初期閉鎖状態へ戻らない"
        )
        steps.append(make_step(
            f"{name}_check", "fail", reason=reason, delimiter=delimiter,
            initial_screenshot=str(initial_image), unclosed_screenshot=str(unclosed_image), closed_screenshot=str(closed_image),
            initial_pixel_digest=initial_digest, unclosed_pixel_digest=unclosed_digest, closed_pixel_digest=closed_digest,
            visual_transition_observed=visual_transition, closed_visual_restored=visual_restored,
            unclosed_expected=unclosed_expected, unclosed_actual=unclosed_actual,
            closed_expected=closed_expected, closed_actual=closed_actual,
        ))
        steps.append(make_step(name, "fail", reason=reason))
        return steps
    steps.append(make_step(
        f"{name}_check", "pass", delimiter=delimiter,
        initial_screenshot=str(initial_image), unclosed_screenshot=str(unclosed_image), closed_screenshot=str(closed_image),
        initial_pixel_digest=initial_digest, unclosed_pixel_digest=unclosed_digest, closed_pixel_digest=closed_digest,
        visual_transition_observed=True, closed_visual_restored=True,
        unclosed_expected=unclosed_expected, unclosed_actual=unclosed_actual,
        closed_expected=closed_expected, closed_actual=closed_actual,
    ))
    for index in range(3):
        ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"区切り記号テストの復元 undo {index + 1} に失敗した: {err}"))
            return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, INLINE_FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout)
    if not matched:
        steps.append(make_step(name, "fail", reason=f"{delimiter!r} テスト後に元の内容へ戻らない: actual={_decode(actual)!r}"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_reset")
    steps.append(reset)
    steps.append(make_step(name, "pass" if reset["result"] == "pass" else "blocked", reason=reset.get("reason")))
    return steps


def run_multiline_code_span_toggle_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                                        helper_timeout, poll_timeout) -> list[dict]:
    name = "multiline_code_span_close_toggle"
    pid = current_pid(process_holder)
    if pid is None or window_id is None:
        return [skipped_step(name, "対象プロセスの PID またはウィンドウを取得できなかった")]
    steps: list[dict] = []
    unclosed_expected = INLINE_FIXTURE_ORIGINAL.replace("span`", "span", 1)
    closed_expected = INLINE_FIXTURE_ORIGINAL
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_initial_reset")
    steps.append(reset)
    if reset["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=reset.get("reason")))
        return steps
    initial_capture = capture_named(module, env, config, window_id, run_dir, f"{name}_initial_closed")
    steps.append(initial_capture)
    if initial_capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=initial_capture.get("reason")))
        return steps
    initial_image = run_dir / f"{name}_initial_closed.png"
    initial_digest, digest_error = image_pixel_digest(swift_helper, initial_image, helper_timeout)
    if digest_error:
        steps.append(make_step(name, "blocked", reason=f"初期閉鎖状態の描画 digest 取得に失敗した: {digest_error}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(initial_image), CODE_SPAN_CLOSE_OCR_RE, "end"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"複数行 code span の閉じ backtick の位置決めに失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["shift-select", str(pid), "right", "1"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ backtick の選択に失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["delete-selection-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ backtick の削除に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, unclosed_expected.encode("utf-8"), poll_timeout)
    unclosed_actual = _decode(actual)
    if not matched:
        steps.append(make_step(name, "fail", reason="複数行 code span の閉じ backtick 削除後の内容が期待値と一致しない"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_unclosed_reset")
    steps.append(reset)
    if reset["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=reset.get("reason")))
        return steps
    unclosed_capture = capture_named(module, env, config, window_id, run_dir, f"{name}_unclosed")
    steps.append(unclosed_capture)
    if unclosed_capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=f"未閉鎖状態の撮影に失敗した: {unclosed_capture.get('reason')}"))
        return steps
    unclosed_image = run_dir / f"{name}_unclosed.png"
    unclosed_digest, digest_error = image_pixel_digest(swift_helper, unclosed_image, helper_timeout)
    if digest_error:
        steps.append(make_step(name, "blocked", reason=f"未閉鎖状態の描画 digest 取得に失敗した: {digest_error}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(unclosed_image), CODE_SPAN_CLOSE_OCR_RE, "end"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"再閉鎖位置の位置決めに失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["type-save", str(pid), "`"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ backtick の再入力に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, closed_expected.encode("utf-8"), poll_timeout)
    closed_actual = _decode(actual)
    if not matched:
        steps.append(make_step(name, "fail", reason="閉じ backtick 再入力後の内容が期待値と一致しない"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_closed_reset")
    steps.append(reset)
    if reset["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=reset.get("reason")))
        return steps
    closed_capture = capture_named(module, env, config, window_id, run_dir, f"{name}_closed")
    steps.append(closed_capture)
    if closed_capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=f"再閉鎖状態の撮影に失敗した: {closed_capture.get('reason')}"))
        return steps
    closed_image = run_dir / f"{name}_closed.png"
    closed_digest, digest_error = image_pixel_digest(swift_helper, closed_image, helper_timeout)
    if digest_error:
        steps.append(make_step(name, "blocked", reason=f"再閉鎖状態の描画 digest 取得に失敗した: {digest_error}"))
        return steps
    visual_transition = unclosed_digest != closed_digest
    visual_restored = initial_digest == closed_digest
    if not visual_transition or not visual_restored:
        reason = (
            "複数行 code span の未閉鎖/再閉鎖で描画ピクセルが変化しない"
            if not visual_transition else "複数行 code span の再閉鎖後の描画が初期閉鎖状態へ戻らない"
        )
        steps.append(make_step(
            f"{name}_check", "fail", reason=reason,
            initial_screenshot=str(initial_image), unclosed_screenshot=str(unclosed_image), closed_screenshot=str(closed_image),
            initial_pixel_digest=initial_digest, unclosed_pixel_digest=unclosed_digest, closed_pixel_digest=closed_digest,
            visual_transition_observed=visual_transition, closed_visual_restored=visual_restored,
            unclosed_expected=unclosed_expected, unclosed_actual=unclosed_actual,
            closed_expected=closed_expected, closed_actual=closed_actual,
        ))
        steps.append(make_step(name, "fail", reason=reason))
        return steps
    steps.append(make_step(
        f"{name}_check", "pass",
        initial_screenshot=str(initial_image), unclosed_screenshot=str(unclosed_image), closed_screenshot=str(closed_image),
        initial_pixel_digest=initial_digest, unclosed_pixel_digest=unclosed_digest, closed_pixel_digest=closed_digest,
        visual_transition_observed=True, closed_visual_restored=True,
        unclosed_expected=unclosed_expected, unclosed_actual=unclosed_actual,
        closed_expected=closed_expected, closed_actual=closed_actual,
    ))
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_reset")
    steps.append(reset)
    steps.append(make_step(name, "pass" if reset["result"] == "pass" else "blocked", reason=reset.get("reason")))
    return steps


def run_inline_syntax_scenario(module, env, target_dir, swift_helper, base_run_dir, binary_path,
                               expected_sha, request_id, startup_timeout, window_timeout,
                               helper_timeout, poll_timeout, priority) -> dict:
    run_dir = base_run_dir / "inline_syntax_boundary"
    fixture_path = run_dir / "inline-fixture.md"
    run_dir.mkdir(parents=True, exist_ok=True)
    fixture_path.write_text(INLINE_FIXTURE_ORIGINAL, encoding="utf-8")
    steps: list[dict] = []
    process_holder: dict = {"process": None}
    config = make_config(
        module, workspace_dir=target_dir, scenario="inline-syntax-boundary",
        expected_sha=expected_sha, request_id=request_id, generation="1", run_dir=run_dir,
        fixture_path=fixture_path, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
    )
    try:
        session_steps, window_id = open_session(module, env, config, binary_path, process_holder, "before")
        steps += session_steps
        for name, checks in (
            ("boundary_click_edit_bold_italic", [
                (BOLD_ITALIC_OCR_RE, "start", BOLD_ITALIC_OPEN_RE, "end"),
                (BOLD_ITALIC_OCR_RE, "end", BOLD_ITALIC_CLOSE_RE, "start"),
            ]),
            ("boundary_click_edit_code_span", [
                (CODE_SPAN_OPEN_OCR_RE, "start", CODE_SPAN_OPEN_RE, "end"),
                (CODE_SPAN_CLOSE_OCR_RE, "end", CODE_SPAN_CLOSE_RE, "start"),
            ]),
            ("boundary_click_edit_quote", [(QUOTE_BOLD_OPEN_OCR_RE, "start", QUOTE_BOLD_OPEN_RE, "end")]),
            ("boundary_click_edit_list", [(LIST_ITALIC_OPEN_OCR_RE, "start", LIST_ITALIC_OPEN_RE, "end")]),
        ):
            steps.extend(run_boundary_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                                           name, checks, helper_timeout, poll_timeout))
        steps.extend(run_boundary_navigation_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                                                  helper_timeout, poll_timeout))
        steps.extend(run_boundary_ime_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                                           helper_timeout, poll_timeout))
        steps.extend(run_drag_select_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                                          helper_timeout, poll_timeout))
        for kind, delimiter in (("star", "*"), ("bold", "**"), ("code", "`")):
            steps.extend(run_delimiter_toggle_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                                                   kind, delimiter, helper_timeout, poll_timeout))
        steps.extend(run_multiline_code_span_toggle_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                                                         helper_timeout, poll_timeout))
        if window_id is not None:
            steps.append(capture_named(module, env, config, window_id, run_dir, "after"))
    finally:
        steps.append(close_session(module, env, process_holder))

    if worst_result(steps, priority) != "pass":
        for name in ("launch_reopen", "window_discovery_reopen", "capture_reopen",
                     "visible_saved_text", "reopen_content_check", "cleanup_reopen"):
            steps.append(skipped_step(name, "再オープン前の工程が pass しなかった"))
        return {"name": "inline_syntax_boundary", "steps": steps,
                "result": worst_result(steps, priority), "reason": reason_for(steps),
                "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}

    expected_final = INLINE_FIXTURE_ORIGINAL
    reopen_dir = run_dir / "reopen"
    reopen_process_holder: dict = {"process": None}
    reopen_config = make_config(
        module, workspace_dir=target_dir, scenario="inline-syntax-boundary-reopen",
        expected_sha=expected_sha, request_id=request_id, generation="2", run_dir=reopen_dir,
        fixture_path=fixture_path, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
    )
    try:
        launch_step = module.do_launch(env, reopen_config, binary_path, reopen_process_holder)
        steps.append({**launch_step, "name": "launch_reopen"})
        reopen_window_id = None
        if launch_step["result"] != "pass":
            steps.append(skipped_step("window_discovery_reopen", "launch_reopen が pass しなかった"))
            steps.append(skipped_step("capture_reopen", "launch_reopen が pass しなかった"))
        else:
            window_step, reopen_window_id = module.do_window_discovery(env, reopen_config, reopen_process_holder["process"])
            steps.append({**window_step, "name": "window_discovery_reopen"})
            if window_step["result"] != "pass":
                steps.append(skipped_step("capture_reopen", "window_discovery_reopen が pass しなかった"))
            else:
                steps.append(capture_named(module, env, reopen_config, reopen_window_id, reopen_dir, "reopen"))
        steps.append(verify_visible_text(swift_helper, reopen_dir / "reopen.png", "bold italic combo", helper_timeout))
        matched, actual = wait_for_fixture_bytes(fixture_path, expected_final.encode("utf-8"), 1.0)
        steps.append(make_step(
            "reopen_content_check", "pass" if matched else "fail",
            reason=None if matched else "再オープン後もフィクスチャ内容が期待通りであることを確認できない",
            note="ファイル内容の一致は再オープンの証跡の一部に過ぎず、描画結果そのものの証明ではない",
            actual=_decode(actual),
        ))
    finally:
        cleanup_step = close_session(module, env, reopen_process_holder)
        steps.append({**cleanup_step, "name": "cleanup_reopen"})
    return {"name": "inline_syntax_boundary", "steps": steps,
            "result": worst_result(steps, priority), "reason": reason_for(steps),
            "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}


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
    base_run_dir = Path(os.environ.get("HANE_GUI_INTERACTION_RUN_DIR") or (Path(tempfile.gettempdir()) / "hane-gui-interaction"))
    base_run_dir.mkdir(parents=True, exist_ok=True)
    request_id = os.environ.get("HANE_GUI_INTERACTION_REQUEST_ID") or (time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-{os.getpid()}")
    expected_sha = os.environ.get("HANE_GUI_INTERACTION_EXPECTED_SHA", "")
    if not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        raise ValueError("full expected target SHA is required")
    startup_timeout = env_float("HANE_GUI_INTERACTION_STARTUP_TIMEOUT_SECS", 15.0)
    window_timeout = env_float("HANE_GUI_INTERACTION_WINDOW_TIMEOUT_SECS", 30.0)
    helper_timeout = env_float("HANE_GUI_INTERACTION_HELPER_TIMEOUT_SECS", 20.0)
    poll_timeout = env_float("HANE_GUI_INTERACTION_POLL_TIMEOUT_SECS", 10.0)
    os.chdir(target_dir)
    module = load_pinned_gui_validate(control_dir)
    env = module.RealEnvironment()
    priority = module.RESULT_PRIORITY
    lock_error = None
    helper_directory = tempfile.TemporaryDirectory(prefix="hane-trusted-helper-")
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
            try:
                swift_helper = prepare_helper(swift_helper, Path(helper_directory.name))
                top_steps.append(make_step("prepare_helper", "pass", sha256=swift_helper.digest))
            except (OSError, subprocess.SubprocessError) as exc:
                top_steps.append(make_step("prepare_helper", "blocked", reason=str(exc)))
            if isinstance(swift_helper, PreparedHelper):
                build_step, binary_path, build_info = module.do_build(env, preflight_config)
            else:
                build_step, binary_path = skipped_step("build", "trusted helper unavailable"), None
            top_steps.append(build_step)
            if build_step["result"] != "pass":
                binary_path = None
            if binary_path is not None:
                def handle_signal(signum, _frame):
                    raise module.Aborted(f"signal {signum}")
                previous = {sig: signal.signal(sig, handle_signal) for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT)}
                try:
                    for scenario_name, run_scenario in (
                        ("ascii_edit_save_undo_redo_reopen", run_ascii_scenario),
                        ("japanese_ime_input", run_ime_scenario),
                        ("os_scroll", run_scroll_scenario),
                        ("inline_syntax_boundary", run_inline_syntax_scenario),
                    ):
                        try:
                            scenarios.append(run_scenario(
                                module, env, target_dir, swift_helper, base_run_dir, binary_path,
                                expected_sha, request_id, startup_timeout, window_timeout,
                                helper_timeout, poll_timeout, priority,
                            ))
                        except (module.Aborted, Exception) as exc:
                            scenarios.append({"name": scenario_name, "result": "blocked", "reason": str(exc), "steps": []})
                            if isinstance(exc, module.Aborted):
                                break
                finally:
                    for sig, handler in previous.items():
                        signal.signal(sig, handler)
        scenario_results = [s["result"] for s in scenarios] or ["blocked"]
        all_results = [s["result"] for s in top_steps if s["result"] in priority] + [r for r in scenario_results if r in priority]
        overall_result = min(all_results, key=lambda r: priority[r]) if all_results else "blocked"
        reasons = [s.get("reason") for s in top_steps if s.get("reason") and s["result"] != "pass"]
        reasons += [s.get("reason") for s in scenarios if s.get("reason") and s["result"] != "pass"]
        overall_reason = "; ".join(r for r in reasons if r) or "すべての工程が成功した"
        result_doc = {
            "schema_version": SCHEMA_VERSION, "procedure_version": PROCEDURE_VERSION,
            "verification_kind": VERIFICATION_KIND, "request_id": request_id,
            "run_id": os.environ.get("GITHUB_RUN_ID", ""), "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT", ""),
            "started_at": started_at, "finished_at": env.clock.now_iso(), "target": target_info,
            "control": {"sha": control_sha},
            "runner": {"os": os.environ.get("RUNNER_OS", ""), "arch": os.environ.get("RUNNER_ARCH", ""),
                       "image_os": os.environ.get("ImageOS", ""), "image_version": os.environ.get("ImageVersion", ""),
                       "macos_version": platform.mac_ver()[0], "machine": platform.machine()},
            "build": build_info, "top_level_steps": top_steps, "scenarios": scenarios,
            "overall_result": overall_result, "overall_reason": overall_reason, "scope_note": SCOPE_NOTE,
        }
        label = {"pass": "PASS", "fail": "FAIL", "blocked": "BLOCKED"}[overall_result]
        result_doc["summary"] = f"[{label}] hosted-gui-interaction request={request_id} — {overall_reason} (対話スモーク確認。網羅的な GUI 検証ではない)"
        result_path = base_run_dir / "result.json"
        result_path.write_text(json.dumps(result_doc, indent=2, ensure_ascii=False) + "\n")
        (base_run_dir / "summary.md").write_text(result_doc["summary"] + "\n")
        print(result_doc["summary"])
        print(f"result: {result_path}")
        return EXIT_PASS if overall_result == "pass" else EXIT_NONPASS
    finally:
        env.release_execution()
        helper_directory.cleanup()


if __name__ == "__main__":
    sys.exit(main())
