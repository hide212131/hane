#!/usr/bin/env python3
"""Focused hosted GUI validation for Issue #195 sidebar chrome behavior."""

from __future__ import annotations

from dataclasses import replace
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "hosted-sidebar-chrome/1"
VERIFICATION_KIND = "sidebar_chrome_focused"
SCOPE_NOTE = (
    "Issue #195 の sidebar divider / transient scrollbar / resize だけを検証する "
    "focused GUI evidence。comprehensive GUI validation の合格を意味しない。"
)
RESULT_PRIORITY = {"pass": 0, "skipped": 1, "blocked": 2, "fail": 3}


def load_gui_validate(control_dir: Path):
    path = control_dir / "scripts" / "gui_validate.py"
    spec = importlib.util.spec_from_file_location("trusted_gui_validate", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("trusted gui_validate.py を読み込めない")
    module = importlib.util.module_from_spec(spec)
    sys.modules["trusted_gui_validate"] = module
    spec.loader.exec_module(module)
    return module


def step(name: str, result: str, reason: str | None = None, **extra):
    return {"name": name, "result": result, "reason": reason, **extra}


def worst_result(steps: list[dict]) -> str:
    if not steps:
        return "blocked"
    return max((s.get("result", "blocked") for s in steps), key=lambda x: RESULT_PRIORITY.get(x, 2))


def reason_for(steps: list[dict]) -> str | None:
    reasons = [s.get("reason") for s in steps if s.get("result") in {"fail", "blocked"} and s.get("reason")]
    return "; ".join(reasons) if reasons else None


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def compile_helper(source: Path, directory: Path) -> tuple[Path, str]:
    binary = directory / "sidebar-chrome-helper"
    completed = subprocess.run(
        ["/usr/bin/swiftc", str(source), "-o", str(binary)],
        capture_output=True,
        text=True,
        timeout=120,
    )
    if completed.returncode:
        raise RuntimeError(completed.stderr.strip() or "swift helper build failed")
    return binary, sha256(binary)


def helper_json(helper: Path, digest: str, args: list[str], timeout: float = 15.0) -> dict:
    if sha256(helper) != digest:
        raise RuntimeError("trusted helper digest changed")
    completed = subprocess.run(
        [str(helper), *args], capture_output=True, text=True, timeout=timeout
    )
    if completed.returncode:
        raise RuntimeError(completed.stderr.strip() or completed.stdout.strip() or "helper failed")
    try:
        payload = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError("helper returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("helper JSON must be an object")
    return payload


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
        image_path=run_dir / "idle.png",
        fixture_path=fixture,
        features=["timing-probe"],
        extra_env={},
        startup_timeout_seconds=20.0,
        window_timeout_seconds=30.0,
    )


def make_fixtures(folder: Path, count: int = 80) -> list[str]:
    folder.mkdir(parents=True, exist_ok=False)
    for index in range(1, count + 1):
        (folder / f"N{index:03d}.md").write_text(f"sidebar chrome fixture {index}\n", encoding="utf-8")
    return sorted(path.name for path in folder.iterdir())


def require_metric(metrics: dict, key: str, kind):
    value = metrics.get(key)
    if isinstance(value, bool) or not isinstance(value, kind):
        raise RuntimeError(f"invalid helper metric: {key}")
    return value


def validate_metrics(metrics: dict) -> dict:
    width = require_metric(metrics, "width", int)
    height = require_metric(metrics, "height", int)
    divider_x = require_metric(metrics, "divider_x", int)
    divider_width = require_metric(metrics, "divider_width", int)
    thumb_width = require_metric(metrics, "thumb_width", int)
    max_run = require_metric(metrics, "max_non_background_run", int)
    if width <= 0 or height <= 0:
        raise RuntimeError("invalid image size")
    if not (0 <= divider_x < width):
        raise RuntimeError("divider_x is outside image")
    if not (0 <= divider_width <= 8 and 0 <= thumb_width <= 20 and 0 <= max_run <= height):
        raise RuntimeError("helper metric outside trusted bounds")
    return {
        "width": width,
        "height": height,
        "divider_x": divider_x,
        "divider_width": divider_width,
        "thumb_width": thumb_width,
        "max_non_background_run": max_run,
        "sidebar_background": metrics.get("sidebar_background"),
        "main_background": metrics.get("main_background"),
        "active_columns": metrics.get("active_columns", []),
    }


def idle_acceptance(metrics: dict) -> tuple[bool, str | None]:
    valid = validate_metrics(metrics)
    if not (1 <= valid["divider_width"] <= 2):
        return False, f"divider が細線ではない: width={valid['divider_width']}"
    if valid["thumb_width"] != 0:
        return False, f"idle 時に scrollbar track/thumb が残っている: width={valid['thumb_width']}"
    return True, None


def scrolling_acceptance(metrics: dict) -> tuple[bool, str | None]:
    valid = validate_metrics(metrics)
    if not (2 <= valid["thumb_width"] <= 4):
        return False, f"scroll 中 thumb が 2〜4px の範囲ではない: width={valid['thumb_width']}"
    if valid["max_non_background_run"] < 20:
        return False, f"scroll thumb の連続長が短すぎる: run={valid['max_non_background_run']}"
    return True, None


def main() -> int:
    target_value = os.environ.get("HANE_SIDEBAR_CHROME_GUI_TARGET_DIR")
    control_value = os.environ.get("HANE_SIDEBAR_CHROME_GUI_CONTROL_DIR")
    expected_sha = os.environ.get("HANE_SIDEBAR_CHROME_GUI_EXPECTED_SHA", "")
    if not target_value or not control_value or not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        print("target dir, control dir and full expected SHA are required", file=sys.stderr)
        return 3

    target_dir = Path(target_value).resolve()
    control_dir = Path(control_value).resolve()
    run_dir = Path(
        os.environ.get("HANE_SIDEBAR_CHROME_GUI_RUN_DIR")
        or (Path(tempfile.gettempdir()) / "hane-sidebar-chrome-gui")
    ).resolve()
    request_id = os.environ.get("HANE_SIDEBAR_CHROME_GUI_REQUEST_ID") or f"sidebar-chrome-{os.getpid()}"
    run_dir.mkdir(parents=True, exist_ok=True)
    result_path = run_dir / "result.json"
    module = load_gui_validate(control_dir)
    env = module.RealEnvironment()
    started_at = env.clock.now_iso()
    top_steps: list[dict] = []
    scenario_steps: list[dict] = []
    target_info: dict = {}
    build_info: dict = {}
    evidence: dict = {}
    holder = {"process": None}
    helper_tmp = tempfile.TemporaryDirectory(prefix="hane-sidebar-chrome-helper-")

    try:
        env.acquire_execution()
        preflight_config = make_config(module, target_dir, run_dir / "preflight", target_dir, expected_sha, request_id)
        preflight, target_info = module.do_preflight(env, preflight_config)
        top_steps.append(preflight)
        if preflight["result"] == "pass":
            try:
                helper, helper_digest = compile_helper(
                    control_dir / "scripts" / "hosted_sidebar_chrome_gui.swift",
                    Path(helper_tmp.name),
                )
                top_steps.append(step("prepare_helper", "pass", sha256=helper_digest))
            except Exception as exc:
                helper = None
                helper_digest = ""
                top_steps.append(step("prepare_helper", "blocked", str(exc)))

            build_step, binary_path, build_info = module.do_build(
                env, preflight_config, target_info["actual_sha"]
            )
            top_steps.append(build_step)

            if build_step["result"] == "pass" and helper is not None:
                scenario_dir = run_dir / "sidebar_chrome"
                fixture_dir = scenario_dir / "work-folder"
                before_names = make_fixtures(fixture_dir)
                before_contents = {
                    name: (fixture_dir / name).read_bytes() for name in before_names
                }
                config = make_config(module, target_dir, scenario_dir, fixture_dir, expected_sha, request_id)
                try:
                    launch = module.do_launch(env, config, binary_path, holder)
                    scenario_steps.append(launch)
                    process = holder["process"]
                    if launch["result"] == "pass":
                        window_step, window_id = module.do_window_discovery(env, config, process)
                        scenario_steps.append(window_step)
                        if window_step["result"] == "pass":
                            time.sleep(1.0)
                            idle_capture = module.do_capture(env, config, window_id)
                            scenario_steps.append({**idle_capture, "name": "capture_idle"})
                            if idle_capture["result"] == "pass":
                                try:
                                    idle = validate_metrics(helper_json(
                                        helper, helper_digest, ["analyze", str(config.image_path)]
                                    ))
                                    ok, reason = idle_acceptance(idle)
                                    scenario_steps.append(step("idle_thin_boundary", "pass" if ok else "fail", reason, metrics=idle))
                                    evidence["idle"] = {"screenshot": str(config.image_path), "metrics": idle}

                                    pid = getattr(process, "pid", None)
                                    if not isinstance(pid, int) or pid <= 0:
                                        scenario_steps.append(step("scrolling_thumb", "blocked", "target PID を取得できない"))
                                    else:
                                        scroll_image = scenario_dir / "scrolling.png"
                                        scrolling = validate_metrics(helper_json(
                                            helper,
                                            helper_digest,
                                            ["wheel-capture-analyze", str(pid), "-600", str(scroll_image)],
                                        ))
                                        ok, reason = scrolling_acceptance(scrolling)
                                        scenario_steps.append(step("scrolling_thumb", "pass" if ok else "fail", reason, metrics=scrolling))
                                        evidence["scrolling"] = {"screenshot": str(scroll_image), "metrics": scrolling}

                                        time.sleep(0.8)
                                        hidden_cfg = replace(config, image_path=scenario_dir / "after-hide.png")
                                        hidden_capture = module.do_capture(env, hidden_cfg, window_id)
                                        scenario_steps.append({**hidden_capture, "name": "capture_after_hide"})
                                        if hidden_capture["result"] == "pass":
                                            hidden = validate_metrics(helper_json(
                                                helper, helper_digest, ["analyze", str(hidden_cfg.image_path)]
                                            ))
                                            ok, reason = idle_acceptance(hidden)
                                            if ok and abs(hidden["divider_x"] - idle["divider_x"]) > 1:
                                                ok, reason = False, "scroll hide 後に divider 位置が予期せず変化した"
                                            scenario_steps.append(step("scrollbar_hides", "pass" if ok else "fail", reason, metrics=hidden))
                                            evidence["after_hide"] = {"screenshot": str(hidden_cfg.image_path), "metrics": hidden}

                                            start_fraction = (idle["divider_x"] + 1.0) / idle["width"]
                                            drag = helper_json(
                                                helper,
                                                helper_digest,
                                                ["drag-divider", str(pid), f"{start_fraction:.8f}", "40"],
                                            )
                                            scenario_steps.append(step(
                                                "resize_drag_event", "pass", None,
                                                start_offset_from_visual_line_px=1.0,
                                                event=drag,
                                            ))
                                            time.sleep(0.2)
                                            resize_cfg = replace(config, image_path=scenario_dir / "after-resize.png")
                                            resize_capture = module.do_capture(env, resize_cfg, window_id)
                                            scenario_steps.append({**resize_capture, "name": "capture_after_resize"})
                                            if resize_capture["result"] == "pass":
                                                resized = validate_metrics(helper_json(
                                                    helper, helper_digest, ["analyze", str(resize_cfg.image_path)]
                                                ))
                                                shift = resized["divider_x"] - idle["divider_x"]
                                                resize_ok, resize_reason = idle_acceptance(resized)
                                                if resize_ok and not (28 <= shift <= 52):
                                                    resize_ok = False
                                                    resize_reason = f"divider が期待範囲で移動しない: shift={shift}px"
                                                scenario_steps.append(step(
                                                    "resize_usable_hit_area",
                                                    "pass" if resize_ok else "fail",
                                                    resize_reason,
                                                    divider_shift_px=shift,
                                                    metrics=resized,
                                                ))
                                                evidence["after_resize"] = {
                                                    "screenshot": str(resize_cfg.image_path),
                                                    "metrics": resized,
                                                    "divider_shift_px": shift,
                                                }
                                except Exception as exc:
                                    scenario_steps.append(step("focused_measurement", "blocked", str(exc)))
                finally:
                    scenario_steps.append(module.do_cleanup(env, holder["process"]))

                after_names = sorted(path.name for path in fixture_dir.iterdir())
                unchanged = (
                    before_names == after_names
                    and all((fixture_dir / name).read_bytes() == before_contents[name] for name in before_names)
                )
                scenario_steps.append(step(
                    "filesystem_unchanged",
                    "pass" if unchanged else "fail",
                    None if unchanged else "GUI 操作で fixture が変更された",
                    before=before_names,
                    after=after_names,
                ))
        else:
            top_steps.append(step("build", "skipped", "preflight が pass しなかった"))
    except Exception as exc:
        top_steps.append(step("procedure_exception", "blocked", str(exc)))
        try:
            if holder.get("process") is not None:
                scenario_steps.append(module.do_cleanup(env, holder["process"]))
        except Exception:
            pass
    finally:
        helper_tmp.cleanup()
        try:
            env.release_execution()
        except Exception:
            pass

    scenario_result = worst_result(scenario_steps) if scenario_steps else "blocked"
    scenarios = [{
        "name": "sidebar_chrome",
        "result": scenario_result,
        "steps": scenario_steps,
        "evidence": evidence,
    }]
    all_steps = top_steps + scenario_steps
    overall = worst_result(all_steps)
    overall_reason = reason_for(all_steps)
    result = {
        "schema_version": SCHEMA_VERSION,
        "procedure_version": PROCEDURE_VERSION,
        "verification_kind": VERIFICATION_KIND,
        "scope_note": SCOPE_NOTE,
        "request_id": request_id,
        "started_at": started_at,
        "finished_at": env.clock.now_iso(),
        "target": target_info,
        "control": {"sha": os.environ.get("HANE_SIDEBAR_CHROME_GUI_CONTROL_SHA", "")},
        "runner": module.runner_metadata(),
        "build": build_info,
        "top_level_steps": top_steps,
        "scenarios": scenarios,
        "overall_result": overall,
        "overall_reason": overall_reason,
        "summary": f"[{overall.upper()}] {PROCEDURE_VERSION}" + (f" — {overall_reason}" if overall_reason else ""),
    }
    result_path.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(result["summary"])
    return 0 if overall == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
