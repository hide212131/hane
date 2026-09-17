#!/usr/bin/env python3
"""Trusted focused GUI validation for sidebar date badges (Issue #174).

This is deliberately a separate verification kind from hosted-gui-interaction/7.
It proves only the date-badge sidebar acceptance criteria and does not weaken or
replace the comprehensive interactive-input procedure.
"""

from __future__ import annotations

import datetime as dt
import hashlib
import importlib.util
import json
import os
import platform
import re
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "hosted-date-badge/1"
VERIFICATION_KIND = "sidebar_date_badge_focused"
SCOPE_NOTE = (
    "Issue #174 の sidebar date-badge 表示だけを検証する focused GUI evidence。"
    "hosted-gui-interaction/7 の包括的 GUI 検証合格を意味しない。"
)
EXIT_PASS = 0
EXIT_NONPASS = 1


def load_gui_validate(control_dir: Path):
    sys.dont_write_bytecode = True
    module_path = control_dir / "scripts" / "gui_validate.py"
    spec = importlib.util.spec_from_file_location("date_badge_gui_validate", module_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load trusted gui_validate.py: {module_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules["date_badge_gui_validate"] = module
    spec.loader.exec_module(module)
    return module


def step(name: str, result: str, reason: str | None = None, **detail) -> dict:
    return {"name": name, "result": result, "reason": reason, **detail}


def worst(steps: list[dict], priority: dict[str, int]) -> str:
    values = [item["result"] for item in steps if item.get("result") in priority]
    return min(values, key=lambda value: priority[value]) if values else "blocked"


def compile_helper(source: Path, directory: Path) -> tuple[Path, str]:
    binary = directory / "date-badge-vision-helper"
    subprocess.run(
        ["/usr/bin/swiftc", str(source), "-o", str(binary)],
        capture_output=True,
        text=True,
        check=True,
        timeout=120,
    )
    binary.chmod(0o500)
    return binary, hashlib.sha256(binary.read_bytes()).hexdigest()


def helper_find_all(helper: Path, digest: str, screenshot: Path, pattern: str) -> list[dict]:
    if hashlib.sha256(helper.read_bytes()).hexdigest() != digest:
        raise RuntimeError("trusted vision helper integrity mismatch before execution")
    proc = subprocess.run(
        [str(helper), "find-all", str(screenshot), pattern],
        capture_output=True,
        text=True,
        timeout=30,
    )
    if hashlib.sha256(helper.read_bytes()).hexdigest() != digest:
        raise RuntimeError("trusted vision helper integrity mismatch after execution")
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"vision helper exited {proc.returncode}")
    value = json.loads(proc.stdout)
    if not isinstance(value, list):
        raise RuntimeError("vision helper did not return a list")
    return value


def center_y(match: dict) -> float:
    box = match["bounding_box"]
    return (float(box["minY"]) + float(box["maxY"])) / 2.0


def center_x(match: dict) -> float:
    box = match["bounding_box"]
    return (float(box["minX"]) + float(box["maxX"])) / 2.0


def nearest_same_row(text_match: dict, badge_matches: list[dict]) -> dict | None:
    if not badge_matches:
        return None
    candidate = min(badge_matches, key=lambda item: abs(center_y(item) - center_y(text_match)))
    return candidate if abs(center_y(candidate) - center_y(text_match)) <= 0.035 else None


def japanese_weekday(value: dt.date) -> str:
    return "月火水木金土日"[value.weekday()]


def relative_label(value: dt.date, today: dt.date) -> str:
    weekday = japanese_weekday(value)
    if value == today:
        return "本日"
    if value.year == today.year and value.month == today.month:
        return f"{value.day}日({weekday})"
    if value.year == today.year:
        return f"{value.month}/{value.day}({weekday})"
    return f"{value.year}/{value.month}/{value.day}({weekday})"


def make_config(module, target_dir: Path, run_dir: Path, fixture: Path, expected_sha: str, request_id: str):
    state_dir = run_dir / "state"
    state_dir.mkdir(parents=True, exist_ok=True)
    return module.Config(
        workspace_dir=target_dir,
        scenario="editor",
        expected_sha=expected_sha,
        request_id=request_id,
        generation="1",
        run_dir=run_dir,
        state_dir=state_dir,
        log_path=run_dir / "hane.log",
        image_path=run_dir / "sidebar-date-badges.png",
        fixture_path=fixture,
        features=["timing-probe"],
        extra_env={},
        startup_timeout_seconds=20.0,
        window_timeout_seconds=30.0,
    )


def make_fixtures(folder: Path) -> tuple[list[str], list[dict]]:
    folder.mkdir(parents=True, exist_ok=False)
    today = dt.date.today()
    today_token = today.strftime("%Y-%m-%d")
    one_digit = dt.date(today.year, 1, 2)
    cases = [
        {
            "name": "date_at_start",
            "filename": f"{today_token}_Alpha.md",
            "text_pattern": r"Alpha\.md",
            "badge_label": "本日",
        },
        {
            "name": "date_in_middle",
            "filename": f"Bravo_{today_token}_Note.md",
            "text_pattern": r"Bravo\s+Note\.md",
            "badge_label": "本日",
        },
        {
            "name": "date_at_end",
            "filename": f"Charlie_{today_token}.md",
            "text_pattern": r"Charlie\.md",
            "badge_label": "本日",
        },
        {
            "name": "one_digit_month_day",
            "filename": f"{one_digit.year}-1-2_Delta.md",
            "text_pattern": r"Delta\.md",
            "badge_label": relative_label(one_digit, today),
        },
        {
            "name": "long_name_keeps_badge_visible",
            "filename": (
                "This_Is_An_Extremely_Long_Sidebar_Filename_Designed_To_Force_"
                f"Truncation_{today_token}.md"
            ),
            "text_pattern": r"This",
            "badge_label": "本日",
        },
    ]
    for case in cases:
        (folder / case["filename"]).write_text("sidebar date badge fixture\n", encoding="utf-8")
    return sorted(path.name for path in folder.iterdir()), cases


def main() -> int:
    target_value = os.environ.get("HANE_DATE_BADGE_GUI_TARGET_DIR")
    control_value = os.environ.get("HANE_DATE_BADGE_GUI_CONTROL_DIR")
    expected_sha = os.environ.get("HANE_DATE_BADGE_GUI_EXPECTED_SHA", "")
    if not target_value or not control_value or not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        print("target dir, control dir and full expected SHA are required", file=sys.stderr)
        return 3

    target_dir = Path(target_value).resolve()
    control_dir = Path(control_value).resolve()
    run_dir = Path(
        os.environ.get("HANE_DATE_BADGE_GUI_RUN_DIR")
        or (Path(tempfile.gettempdir()) / "hane-date-badge-gui")
    ).resolve()
    request_id = os.environ.get("HANE_DATE_BADGE_GUI_REQUEST_ID") or f"date-badge-{os.getpid()}"
    run_dir.mkdir(parents=True, exist_ok=True)
    result_path = run_dir / "result.json"
    helper_source = control_dir / "scripts" / "hosted_date_badge_gui.swift"
    module = load_gui_validate(control_dir)
    env = module.RealEnvironment()
    priority = module.RESULT_PRIORITY
    top_steps: list[dict] = []
    scenario_steps: list[dict] = []
    target_info: dict = {}
    build_info: dict = {}
    process = None
    started_at = env.clock.now_iso()
    helper_tmp = tempfile.TemporaryDirectory(prefix="hane-date-badge-helper-")

    try:
        env.acquire_execution()
        preflight_config = make_config(module, target_dir, run_dir / "preflight", target_dir, expected_sha, request_id)
        preflight, target_info = module.do_preflight(env, preflight_config)
        top_steps.append(preflight)
        if preflight["result"] != "pass":
            top_steps.append(step("build", "skipped", "preflight が pass しなかった"))
        else:
            try:
                helper, helper_digest = compile_helper(helper_source, Path(helper_tmp.name))
                top_steps.append(step("prepare_helper", "pass", sha256=helper_digest))
            except (OSError, subprocess.SubprocessError) as exc:
                helper = None
                helper_digest = ""
                top_steps.append(step("prepare_helper", "blocked", str(exc)))

            build_step, binary_path, build_info = module.do_build(env, preflight_config, target_info["actual_sha"])
            top_steps.append(build_step)
            if build_step["result"] == "pass" and helper is not None:
                scenario_dir = run_dir / "sidebar_date_badges"
                fixture_dir = scenario_dir / "work-folder"
                before_names, cases = make_fixtures(fixture_dir)
                config = make_config(module, target_dir, scenario_dir, fixture_dir, expected_sha, request_id)
                holder = {"process": None}
                try:
                    launch = module.do_launch(env, config, binary_path, holder)
                    scenario_steps.append(launch)
                    process = holder["process"]
                    if launch["result"] == "pass":
                        window_step, window_id = module.do_window_discovery(env, config, process)
                        scenario_steps.append(window_step)
                        if window_step["result"] == "pass":
                            time.sleep(2.0)
                            capture = module.do_capture(env, config, window_id)
                            scenario_steps.append(capture)
                            if capture["result"] == "pass":
                                screenshot = config.image_path
                                badge_cache: dict[str, list[dict]] = {}
                                for case in cases:
                                    try:
                                        texts = helper_find_all(helper, helper_digest, screenshot, case["text_pattern"])
                                        badges = badge_cache.setdefault(
                                            case["badge_label"],
                                            helper_find_all(helper, helper_digest, screenshot, re.escape(case["badge_label"])),
                                        )
                                        if not texts:
                                            scenario_steps.append(step(case["name"], "fail", "ファイル名本文を screenshot OCR で確認できない"))
                                            continue
                                        text_match = texts[0]
                                        badge_match = nearest_same_row(text_match, badges)
                                        if badge_match is None:
                                            scenario_steps.append(step(case["name"], "fail", "同じ sidebar row の日付バッジを確認できない"))
                                            continue
                                        text_box = text_match["bounding_box"]
                                        badge_box = badge_match["bounding_box"]
                                        right_side = float(badge_box["minX"]) >= float(text_box["maxX"]) - 0.01
                                        scenario_steps.append(step(
                                            case["name"],
                                            "pass" if right_side else "fail",
                                            None if right_side else "日付バッジがファイル名本文の右側にない",
                                            filename=case["filename"],
                                            expected_badge=case["badge_label"],
                                            text_match=text_match,
                                            badge_match=badge_match,
                                            screenshot=str(screenshot),
                                        ))
                                    except (OSError, subprocess.SubprocessError, ValueError, RuntimeError, json.JSONDecodeError) as exc:
                                        scenario_steps.append(step(case["name"], "blocked", str(exc)))
                finally:
                    if process is not None:
                        scenario_steps.append(module.do_cleanup(env, process))

                after_names = sorted(path.name for path in fixture_dir.iterdir())
                unchanged_contents = all(
                    path.read_text(encoding="utf-8") == "sidebar date badge fixture\n"
                    for path in fixture_dir.iterdir() if path.is_file()
                )
                invariant = before_names == after_names and unchanged_contents
                scenario_steps.append(step(
                    "filesystem_unchanged",
                    "pass" if invariant else "fail",
                    None if invariant else "GUI validation 中に fixture の名前/path/content が変化した",
                    before=before_names,
                    after=after_names,
                    contents_unchanged=unchanged_contents,
                ))

        considered = [item for item in top_steps + scenario_steps if item.get("result") in priority]
        overall = min((item["result"] for item in considered), key=lambda value: priority[value]) if considered else "blocked"
        reasons = [item.get("reason") for item in considered if item.get("result") != "pass" and item.get("reason")]
        reason = "; ".join(reasons) if reasons else "Issue #174 focused GUI checks passed"
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
            "scenarios": [{
                "name": "sidebar_date_badges",
                "result": worst(scenario_steps, priority),
                "steps": scenario_steps,
                "evidence": {"screenshot": str(run_dir / "sidebar_date_badges" / "sidebar-date-badges.png")},
            }],
            "overall_result": overall,
            "overall_reason": reason,
        }
        label = {"pass": "PASS", "fail": "FAIL", "blocked": "BLOCKED"}.get(overall, "BLOCKED")
        result["summary"] = f"[{label}] hosted-date-badge/1 — {reason}"
        result_path.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        (run_dir / "summary.md").write_text(result["summary"] + "\n", encoding="utf-8")
        print(result["summary"])
        print(f"result: {result_path}")
        return EXIT_PASS if overall == "pass" else EXIT_NONPASS
    except (module.Aborted, module.EnvError, OSError, subprocess.SubprocessError, ValueError, RuntimeError) as exc:
        fallback = {
            "schema_version": SCHEMA_VERSION,
            "procedure_version": PROCEDURE_VERSION,
            "verification_kind": VERIFICATION_KIND,
            "scope_note": SCOPE_NOTE,
            "request_id": request_id,
            "overall_result": "blocked",
            "overall_reason": str(exc),
            "top_level_steps": top_steps,
            "scenarios": [],
        }
        result_path.write_text(json.dumps(fallback, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"[BLOCKED] hosted-date-badge/1 — {exc}")
        return EXIT_NONPASS
    finally:
        env.release_execution()
        helper_tmp.cleanup()


if __name__ == "__main__":
    sys.exit(main())
