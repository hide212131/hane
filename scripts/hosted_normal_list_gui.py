#!/usr/bin/env python3
"""Trusted focused GUI validation for normal list display (Issue #126).

This is deliberately a separate verification kind from hosted-gui-interaction/7
(scripts/hosted_gui_interaction.py) and from hosted-date-badge/N
(scripts/hosted_date_badge_gui.py). It proves only Issue #126's normal-list
display acceptance criteria:

  B. a mixed nested list (unordered item containing an ordered sublist, and
     an ordered item containing an unordered sublist) renders every item's
     content and preserves nesting order,
  C. a list item with multiple paragraphs and a nested child list still
     returns to the parent list's own numbering afterward,
  D. an ordered list whose source markers are not `1.` and are not
     sequential (e.g. `3.`, `41.`, `100.`) still displays sequential normal
     numbers starting at the first marker's value (3, 4, 5).

It reuses the existing launcher/input stack (scripts/gui_validate.py and
scripts/hosted_gui_interaction.py, including its compiled
scripts/hosted_gui_interaction.swift OS-input helper) rather than inventing a
second one. It does not weaken or replace the comprehensive interactive-input
procedure. All visual assertions go through OCR text (`ocr`, `click-text`,
`drag-select-text`) rather than internal rendering geometry, because product
PR #189 is still changing the internal list layout; this keeps the assertions
modular against that in-flight change.
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
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "hosted-normal-list/2"
VERIFICATION_KIND = "normal_list_focused"
SCOPE_NOTE = (
    "Issue #126 の normal list 表示(B: ネスト混在、C: 複数段落+子リスト+親リストへの復帰、"
    "D: 非1・非連番 ordered ソースの連番表示)だけを検証する focused GUI evidence。"
    "hosted-gui-interaction/7 や hosted-date-badge の包括的/専用 GUI 検証合格を意味しない。"
    "product PR #189 でリスト内部レイアウトが変更され続けている間、幾何座標ではなく "
    "OCR テキストベースの検証に限定し、内部レイアウト型には依存しない。"
    "編集、IME、選択範囲、undo/redo、再オープンはこの focused 手順の判定対象外とする。"
)
EXIT_PASS = 0
EXIT_NONPASS = 1

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


def skipped_step(name: str, reason: str) -> dict:
    return {"name": name, "result": "skipped", "reason": reason}


def worst(steps: list, priority: dict) -> str:
    values = [item["result"] for item in steps if item.get("result") in priority]
    return min(values, key=lambda value: priority[value]) if values else "blocked"


def reason_for(steps: list) -> str:
    reasons = [item.get("reason") for item in steps if item.get("result") != "pass" and item.get("reason")]
    return "; ".join(reasons) if reasons else "すべての工程が成功した"


def normalized_ocr_text(value: str) -> str:
    return re.sub(r"\s+", " ", value).strip()


# ---------------------------------------------------------------------------
# Pure source-byte transforms and OCR/step decision logic. No subprocess, no
# filesystem, no GUI: these are exercised directly by scripts/tests/.
# ---------------------------------------------------------------------------


def expected_sequential_numbers(seed: int, count: int) -> list:
    """The CommonMark ordered-list numbering rule: only the first marker's own
    number sets the start; every following item's own source number is
    ignored for numbering purposes and normal display always increments by 1.
    """
    if count <= 0:
        raise ValueError("count must be positive")
    return [seed + offset for offset in range(count)]


def leading_number_pattern(number: int, content_pattern: str) -> str:
    return rf"(?<!\d){number}\s*[.)]?\s*{content_pattern}"


def line_has_number_token(line: str, number: int) -> bool:
    return re.search(rf"(?<!\d){number}(?!\d)", line) is not None


def find_line(lines: list, pattern: str) -> Optional[str]:
    for line in lines:
        if re.search(pattern, line):
            return line
    return None


def parse_click_evidence(stdout: str) -> dict:
    """click-text の stdout(JSON 1行)から OCR/クリック evidence を取り出す。

    hosted_gui_interaction.py の同名関数と同じ契約(必須フィールド欠落や非 JSON は
    helper 自身の契約違反として ValueError にし、呼び出し側で製品 fail と区別できる
    procedure blocked として扱わせる)。
    """
    try:
        payload = json.loads(stdout)
    except (ValueError, TypeError) as exc:
        raise ValueError(f"click-text の stdout を JSON として解釈できない: {stdout!r}") from exc
    required = {"matched_text", "bounding_box", "window_bounds", "click_point", "edge"}
    missing = required - payload.keys()
    if missing:
        raise ValueError(f"click-text evidence に必須フィールドが不足している({sorted(missing)}): {stdout!r}")
    return payload


# ---------------------------------------------------------------------------
# Fixtures (#126 B/C/D). Deterministic and self-contained: no dependency on
# the current date, unlike the sidebar date-badge fixtures.
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class InitialCheck:
    label: str
    content_pattern: str
    expected_number: Optional[int] = None
    forbidden_number: Optional[int] = None


@dataclass(frozen=True)
class MarkerCheck:
    content_pattern: str
    raw_marker_pattern: str
    normalized_pattern: Optional[str] = None


@dataclass(frozen=True)
class ScenarioSpec:
    name: str
    fixture_filename: str
    fixture_text: str
    initial_checks: tuple
    marker_check: MarkerCheck


def build_mixed_nested_list_fixture() -> str:
    return (
        "# Mixed Nested List Fixture\n"
        "\n"
        "- Alpha lead item\n"
        "  1. Alpha nested one\n"
        "  2. Alpha nested two\n"
        "- Beta lead item\n"
        "\n"
        "1. Gamma lead first\n"
        "   - Gamma nested bullet\n"
        "   - Gamma nested second\n"
        "2. Gamma lead second\n"
    )


MIXED_NESTED_LIST_SPEC = ScenarioSpec(
    name="mixed_nested_list",
    fixture_filename="normal-list-mixed-nested.md",
    fixture_text=build_mixed_nested_list_fixture(),
    initial_checks=(
        InitialCheck("alpha_lead", r"Alpha lead item"),
        InitialCheck("alpha_nested_one", r"Alpha nested one"),
        InitialCheck("alpha_nested_two", r"Alpha nested two"),
        InitialCheck("beta_lead", r"Beta lead item"),
        InitialCheck("gamma_lead_first", r"Gamma lead first"),
        InitialCheck("gamma_nested_bullet", r"Gamma nested bullet"),
        InitialCheck("gamma_nested_second", r"Gamma nested second"),
        InitialCheck("gamma_lead_second", r"Gamma lead second"),
    ),
    marker_check=MarkerCheck(
        content_pattern=r"Beta lead item",
        raw_marker_pattern=r"-\s*Beta lead item",
    ),
)


def build_paragraph_child_list_fixture() -> str:
    return (
        "# Paragraph Child List Fixture\n"
        "\n"
        "1. First item intro line\n"
        "\n"
        "   First item continued paragraph.\n"
        "\n"
        "   - Child bullet one\n"
        "   - Child bullet two\n"
        "\n"
        "2. Second item after return\n"
    )


PARAGRAPH_CHILD_LIST_SPEC = ScenarioSpec(
    name="paragraph_child_list",
    fixture_filename="normal-list-paragraph-child.md",
    fixture_text=build_paragraph_child_list_fixture(),
    initial_checks=(
        InitialCheck("first_intro", r"First item intro line"),
        InitialCheck("first_continued", r"First item continued paragraph\."),
        InitialCheck("child_bullet_one", r"Child bullet one"),
        InitialCheck("child_bullet_two", r"Child bullet two"),
        InitialCheck("second_after_return", r"Second item after return"),
    ),
    marker_check=MarkerCheck(
        content_pattern=r"Second item after return",
        raw_marker_pattern=r"2\.\s*Second item after return",
    ),
)


NONSEQUENTIAL_ORDERED_RAW_NUMBERS = (3, 41, 100)
NONSEQUENTIAL_ORDERED_EXPECTED_NUMBERS = tuple(
    expected_sequential_numbers(NONSEQUENTIAL_ORDERED_RAW_NUMBERS[0], len(NONSEQUENTIAL_ORDERED_RAW_NUMBERS))
)


def build_nonsequential_ordered_fixture() -> str:
    return (
        "# Nonsequential Ordered List Fixture\n"
        "\n"
        f"{NONSEQUENTIAL_ORDERED_RAW_NUMBERS[0]}. Alpha row\n"
        f"{NONSEQUENTIAL_ORDERED_RAW_NUMBERS[1]}. Beta row\n"
        f"{NONSEQUENTIAL_ORDERED_RAW_NUMBERS[2]}. Gamma row\n"
    )


NONSEQUENTIAL_ORDERED_SPEC = ScenarioSpec(
    name="nonsequential_ordered_list",
    fixture_filename="normal-list-nonsequential-ordered.md",
    fixture_text=build_nonsequential_ordered_fixture(),
    initial_checks=(
        InitialCheck("alpha_row", r"Alpha row", expected_number=NONSEQUENTIAL_ORDERED_EXPECTED_NUMBERS[0]),
        InitialCheck(
            "beta_row", r"Beta row",
            expected_number=NONSEQUENTIAL_ORDERED_EXPECTED_NUMBERS[1],
            forbidden_number=NONSEQUENTIAL_ORDERED_RAW_NUMBERS[1],
        ),
        InitialCheck(
            "gamma_row", r"Gamma row",
            expected_number=NONSEQUENTIAL_ORDERED_EXPECTED_NUMBERS[2],
            forbidden_number=NONSEQUENTIAL_ORDERED_RAW_NUMBERS[2],
        ),
    ),
    marker_check=MarkerCheck(
        content_pattern=r"Beta row",
        raw_marker_pattern=rf"{NONSEQUENTIAL_ORDERED_RAW_NUMBERS[1]}\.\s*Beta row",
        normalized_pattern=rf"{NONSEQUENTIAL_ORDERED_EXPECTED_NUMBERS[1]}\.\s*Beta row",
    ),
)


ALL_SCENARIO_SPECS = (MIXED_NESTED_LIST_SPEC, PARAGRAPH_CHILD_LIST_SPEC, NONSEQUENTIAL_ORDERED_SPEC)


# ---------------------------------------------------------------------------
# Fail-closed OCR/step evaluation (pure, given already-fetched OCR lines).
# ---------------------------------------------------------------------------


def evaluate_initial_check(lines: list, check: InitialCheck) -> dict:
    line = find_line(lines, check.content_pattern)
    if line is None:
        return step(
            f"initial_{check.label}", "fail",
            f"normal display で '{check.content_pattern}' の内容を OCR で確認できない",
            content_pattern=check.content_pattern, ocr_lines=lines,
        )
    detail = {"content_pattern": check.content_pattern, "matched_line": line}
    if check.expected_number is not None:
        if not re.search(leading_number_pattern(check.expected_number, check.content_pattern), line):
            return step(
                f"initial_{check.label}", "fail",
                f"normal display の番号が期待した連番 {check.expected_number} になっていない",
                expected_number=check.expected_number, **detail,
            )
        detail["expected_number"] = check.expected_number
    if check.forbidden_number is not None and line_has_number_token(line, check.forbidden_number):
        return step(
            f"initial_{check.label}", "fail",
            f"normal display に生のソース番号 {check.forbidden_number} がそのまま表示されている(連番化されていない)",
            forbidden_number=check.forbidden_number, **detail,
        )
    return step(f"initial_{check.label}", "pass", **detail)


def evaluate_marker_disclosure(after_lines: list, check: MarkerCheck) -> dict:
    after_line = find_line(after_lines, check.raw_marker_pattern)
    if after_line is None:
        return step(
            "marker_disclosure", "fail",
            "マーカー付近をクリックしても生の source marker が同じ行で確認できない(disclosure が機能していない疑い)",
            raw_marker_pattern=check.raw_marker_pattern, ocr_lines_after=after_lines,
        )
    return step(
        "marker_disclosure", "pass",
        raw_marker_pattern=check.raw_marker_pattern, matched_line_after=after_line,
    )


def evaluate_marker_reclosure(after_lines: list, check: MarkerCheck) -> dict:
    """Only meaningful when raw source marker and normal display differ.

    #126 D (non-1/non-sequential ordered source) is the only fixture where a
    raw source marker (e.g. `41.`) and its normal-display number (`4.`)
    necessarily differ, so only that fixture's `MarkerCheck` sets
    `normalized_pattern`; otherwise this reports `skipped` rather than
    asserting something the fixture cannot distinguish.
    """
    if check.normalized_pattern is None:
        return skipped_step("marker_reclosure", "raw marker と normal display の表示が区別できない fixture のため対象外")
    still_raw = find_line(after_lines, check.raw_marker_pattern)
    if still_raw is not None:
        return step(
            "marker_reclosure", "fail",
            "caret を離した後も生の source marker が表示されたままで、連番化された表示へ戻っていない",
            raw_marker_pattern=check.raw_marker_pattern, matched_line=still_raw,
        )
    normalized_line = find_line(after_lines, check.normalized_pattern)
    if normalized_line is None:
        return step(
            "marker_reclosure", "fail",
            "caret を離した後に連番化された normal display の番号が確認できない",
            normalized_pattern=check.normalized_pattern, ocr_lines=after_lines,
        )
    return step("marker_reclosure", "pass", normalized_pattern=check.normalized_pattern, matched_line=normalized_line)


# ---------------------------------------------------------------------------
# GUI-driving code. Not unit tested (requires a real macOS hosted runner);
# only calls into scripts/gui_validate.py and scripts/hosted_gui_interaction.py
# primitives loaded dynamically from the trusted control checkout.
# ---------------------------------------------------------------------------


def ocr_lines(interaction_module, helper, screenshot: Path, timeout: float) -> tuple:
    ok, text, error = interaction_module.run_helper(helper, ["ocr", str(screenshot)], timeout)
    if not ok:
        return False, [], error
    return True, [normalized_ocr_text(line) for line in text.splitlines() if line.strip()], ""


def click_content(interaction_module, helper, pid, screenshot: Path, pattern: str, edge: str, timeout: float) -> tuple:
    ok, out, err = interaction_module.run_helper(
        helper, ["click-text", str(pid), str(screenshot), pattern, edge], timeout
    )
    if not ok:
        return False, None, err
    try:
        evidence = parse_click_evidence(out)
    except ValueError as exc:
        return False, None, str(exc)
    return True, evidence, ""


def run_scenario(
    gui_validate_module, interaction_module, env, target_dir: Path, swift_helper, base_run_dir: Path,
    binary_path: Path, expected_sha: str, request_id: str, startup_timeout: float, window_timeout: float,
    helper_timeout: float, priority: dict, spec: ScenarioSpec,
) -> dict:
    run_dir = base_run_dir / spec.name
    fixture_path = run_dir / spec.fixture_filename
    run_dir.mkdir(parents=True, exist_ok=True)
    fixture_path.write_text(spec.fixture_text, encoding="utf-8")
    steps: list = []
    process_holder: dict = {"process": None}
    config = interaction_module.make_config(
        gui_validate_module, workspace_dir=target_dir, scenario=f"normal-list-{spec.name}",
        expected_sha=expected_sha, request_id=request_id, generation="1", run_dir=run_dir,
        fixture_path=fixture_path, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
    )
    try:
        session_steps, window_id = interaction_module.open_session(
            gui_validate_module, env, config, binary_path, process_holder, "before"
        )
        steps += session_steps
        pid = interaction_module.current_pid(process_holder)
        if pid is None or window_id is None:
            steps.append(skipped_step("scenario_body", "launch/window discovery が pass しなかった"))
        else:
            before_screenshot = run_dir / "before.png"
            ok, lines, err = ocr_lines(interaction_module, swift_helper, before_screenshot, helper_timeout)
            if not ok:
                steps.append(step("initial_display_ocr", "blocked", err))
            else:
                steps.append(step("initial_display_ocr", "pass", ocr_lines=lines))
                for check in spec.initial_checks:
                    steps.append(evaluate_initial_check(lines, check))

            marker_check = spec.marker_check
            clicked, evidence, err = click_content(
                interaction_module, swift_helper, pid, before_screenshot,
                marker_check.content_pattern, "start", helper_timeout,
            )
            if not clicked:
                steps.append(step("marker_click", "blocked", err, content_pattern=marker_check.content_pattern))
                steps.append(skipped_step("marker_disclosure", "marker click に失敗した"))
                steps.append(skipped_step("marker_reclosure", "marker click に失敗した"))
            else:
                steps.append(step("marker_click", "pass", click_evidence=evidence))
                steps.append(interaction_module.capture_named(gui_validate_module, env, config, window_id, run_dir, "disclosed"))
                ok, disclosed_lines, err = ocr_lines(interaction_module, swift_helper, run_dir / "disclosed.png", helper_timeout)
                if not ok:
                    steps.append(step("marker_disclosure", "blocked", err))
                    steps.append(skipped_step("marker_reclosure", "disclosure 確認前の OCR が失敗した"))
                else:
                    steps.append(evaluate_marker_disclosure(disclosed_lines, marker_check))
                    ok, _out, err = interaction_module.run_helper(swift_helper, ["move-doc-start", str(pid)], helper_timeout)
                    if not ok:
                        steps.append(step("marker_reclosure_move", "blocked", err))
                        steps.append(skipped_step("marker_reclosure", "caret を戻せなかった"))
                    else:
                        steps.append(interaction_module.capture_named(gui_validate_module, env, config, window_id, run_dir, "reclosed"))
                        ok, reclosed_lines, err = ocr_lines(interaction_module, swift_helper, run_dir / "reclosed.png", helper_timeout)
                        if not ok:
                            steps.append(step("marker_reclosure", "blocked", err))
                        else:
                            steps.append(evaluate_marker_reclosure(reclosed_lines, marker_check))

            steps.append(interaction_module.capture_named(gui_validate_module, env, config, window_id, run_dir, "after"))
    finally:
        steps.append(interaction_module.close_session(gui_validate_module, env, process_holder))

    return {
        "name": spec.name, "steps": steps, "result": worst(steps, priority), "reason": reason_for(steps),
        "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)},
    }


def env_float(name: str, default: float) -> float:
    value = os.environ.get(name)
    if not value:
        return default
    parsed = float(value)
    if parsed != parsed or parsed <= 0 or parsed == float("inf"):
        raise ValueError(f"{name} must be positive and finite")
    return parsed


def main() -> int:
    target_value = os.environ.get("HANE_NORMAL_LIST_GUI_TARGET_DIR")
    control_value = os.environ.get("HANE_NORMAL_LIST_GUI_CONTROL_DIR")
    expected_sha = os.environ.get("HANE_NORMAL_LIST_GUI_EXPECTED_SHA", "")
    if not target_value or not control_value or not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        print("target dir, control dir and full expected SHA are required", file=sys.stderr)
        return 3

    target_dir = Path(target_value).resolve()
    control_dir = Path(control_value).resolve()
    run_dir = Path(
        os.environ.get("HANE_NORMAL_LIST_GUI_RUN_DIR")
        or (Path(tempfile.gettempdir()) / "hane-normal-list-gui")
    ).resolve()
    request_id = os.environ.get("HANE_NORMAL_LIST_GUI_REQUEST_ID") or f"normal-list-{os.getpid()}"
    startup_timeout = env_float("HANE_NORMAL_LIST_GUI_STARTUP_TIMEOUT_SECS", 15.0)
    window_timeout = env_float("HANE_NORMAL_LIST_GUI_WINDOW_TIMEOUT_SECS", 30.0)
    helper_timeout = env_float("HANE_NORMAL_LIST_GUI_HELPER_TIMEOUT_SECS", 20.0)
    run_dir.mkdir(parents=True, exist_ok=True)
    result_path = run_dir / "result.json"

    gui_validate_module = load_module(control_dir, "scripts/gui_validate.py", "normal_list_gui_validate")
    interaction_module = load_module(control_dir, "scripts/hosted_gui_interaction.py", "normal_list_hosted_gui_interaction")
    env = gui_validate_module.RealEnvironment()
    priority = gui_validate_module.RESULT_PRIORITY

    top_steps: list = []
    scenarios: list = []
    target_info: dict = {}
    build_info: dict = {}
    started_at = env.clock.now_iso()
    helper_tmp = tempfile.TemporaryDirectory(prefix="hane-normal-list-helper-")

    try:
        env.acquire_execution()
        preflight_config = interaction_module.make_config(
            gui_validate_module, workspace_dir=target_dir, scenario="normal-list-preflight",
            expected_sha=expected_sha, request_id=request_id, generation="0", run_dir=run_dir / "_preflight",
            fixture_path=None, features=["timing-probe"], extra_env={},
            startup_timeout=startup_timeout, window_timeout=window_timeout,
        )
        preflight, target_info = gui_validate_module.do_preflight(env, preflight_config)
        top_steps.append(preflight)
        binary_path = None
        swift_helper = None
        if preflight["result"] != "pass":
            top_steps.append(skipped_step("build", "preflight が pass しなかった"))
        else:
            try:
                swift_helper = interaction_module.prepare_helper(
                    control_dir / "scripts" / "hosted_gui_interaction.swift", Path(helper_tmp.name)
                )
                top_steps.append(step("prepare_helper", "pass", sha256=swift_helper.digest))
            except (OSError, subprocess.SubprocessError) as exc:
                swift_helper = None
                top_steps.append(step("prepare_helper", "blocked", str(exc)))

            if swift_helper is None:
                top_steps.append(skipped_step("build", "trusted helper が利用できない"))
            else:
                build_step, binary_path, build_info = gui_validate_module.do_build(env, preflight_config, target_info["actual_sha"])
                top_steps.append(build_step)
                if build_step["result"] != "pass":
                    binary_path = None

            if binary_path is not None and swift_helper is not None:
                for spec in ALL_SCENARIO_SPECS:
                    try:
                        scenarios.append(run_scenario(
                            gui_validate_module, interaction_module, env, target_dir, swift_helper,
                            run_dir, binary_path, expected_sha, request_id,
                            startup_timeout, window_timeout, helper_timeout,
                            priority, spec,
                        ))
                    except (gui_validate_module.Aborted, Exception) as exc:
                        scenarios.append({"name": spec.name, "result": "blocked", "reason": str(exc), "steps": []})
                        if isinstance(exc, gui_validate_module.Aborted):
                            break

        all_results = [s["result"] for s in top_steps if s["result"] in priority] + [
            s["result"] for s in scenarios if s["result"] in priority
        ]
        overall = min(all_results, key=lambda value: priority[value]) if all_results else "blocked"
        reasons = [s.get("reason") for s in top_steps if s.get("reason") and s["result"] != "pass"]
        reasons += [s.get("reason") for s in scenarios if s.get("reason") and s["result"] != "pass"]
        reason = "; ".join(r for r in reasons if r) or "Issue #126 focused normal-list GUI checks passed"
        control_sha = env.git_head(control_dir)
        result = {
            "schema_version": SCHEMA_VERSION,
            "procedure_version": PROCEDURE_VERSION,
            "verification_kind": VERIFICATION_KIND,
            "scope_note": SCOPE_NOTE,
            "request_id": request_id,
            "started_at": started_at,
            "finished_at": env.clock.now_iso(),
            "target": target_info,
            "control": {"sha": control_sha},
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
        label = {"pass": "PASS", "fail": "FAIL", "blocked": "BLOCKED"}.get(overall, "BLOCKED")
        result["summary"] = f"[{label}] {PROCEDURE_VERSION} — {reason}"
        result_path.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        (run_dir / "summary.md").write_text(result["summary"] + "\n", encoding="utf-8")
        print(result["summary"])
        print(f"result: {result_path}")
        return EXIT_PASS if overall == "pass" else EXIT_NONPASS
    except (gui_validate_module.Aborted, gui_validate_module.EnvError, OSError, subprocess.SubprocessError, ValueError, RuntimeError) as exc:
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
        result_path.write_text(json.dumps(fallback, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"[BLOCKED] {PROCEDURE_VERSION} — {exc}")
        return EXIT_NONPASS
    finally:
        env.release_execution()
        helper_tmp.cleanup()


if __name__ == "__main__":
    sys.exit(main())
