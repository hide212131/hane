#!/usr/bin/env python3
"""Trusted focused GUI validation for Issue #195 sidebar chrome.

This procedure verifies only the sidebar acceptance:
- idle shows a thin divider without a persistent wide scrollbar track,
- a real OS wheel over the sidebar briefly reveals a narrow overlay thumb,
- the thumb disappears again after the idle delay,
- a real OS pointer drag can still resize the sidebar.

The target application is observed only through window screenshots and OS input.
The validator/helper itself is loaded from the trusted default-branch control tree.
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
from pathlib import Path
from typing import Any, Optional

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "hosted-sidebar/1"
VERIFICATION_KIND = "sidebar_focused"
SCOPE_NOTE = (
    "Issue #195 の sidebar chrome（idle divider / scrolling thumb / post-scroll hide / resize）"
    "だけを検証する focused GUI evidence。comprehensive GUI 合格を意味しない。"
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


def step(name: str, result: str, reason: Optional[str] = None, **detail) -> dict[str, Any]:
    return {"name": name, "result": result, "reason": reason, **detail}


def skipped(name: str, reason: str) -> dict[str, Any]:
    return step(name, "skipped", reason)


def worst(steps: list[dict[str, Any]], priority: dict[str, int]) -> str:
    values = [item["result"] for item in steps if item.get("result") in priority]
    return min(values, key=lambda value: priority[value]) if values else "blocked"


def reason_for(steps: list[dict[str, Any]]) -> str:
    reasons = [
        item.get("reason")
        for item in steps
        if item.get("result") not in ("pass", "skipped") and item.get("reason")
    ]
    return "; ".join(reasons) if reasons else "Issue #195 focused sidebar GUI checks passed"


def compile_helper(source: Path, directory: Path) -> tuple[Path, str]:
    import hashlib

    binary = directory / "sidebar-helper"
    subprocess.run(
        ["/usr/bin/swiftc", str(source), "-o", str(binary)],
        capture_output=True,
        text=True,
        check=True,
        timeout=120,
    )
    binary.chmod(0o500)
    return binary, hashlib.sha256(binary.read_bytes()).hexdigest()


def run_helper(helper: Path, digest: str, args: list[str], timeout: float = 30.0) -> str:
    import hashlib

    if hashlib.sha256(helper.read_bytes()).hexdigest() != digest:
        raise RuntimeError("trusted sidebar helper integrity mismatch before execution")
    proc = subprocess.run(
        [str(helper), *args], capture_output=True, text=True, timeout=timeout
    )
    if hashlib.sha256(helper.read_bytes()).hexdigest() != digest:
        raise RuntimeError("trusted sidebar helper integrity mismatch after execution")
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"sidebar helper exited {proc.returncode}")
    return proc.stdout.strip()


def helper_json(helper: Path, digest: str, args: list[str]) -> dict[str, Any]:
    raw = run_helper(helper, digest, args)
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"sidebar helper returned invalid JSON: {raw!r}") from exc
    if not isinstance(value, dict):
        raise RuntimeError("sidebar helper JSON is not an object")
    return value


def make_fixtures(folder: Path, count: int = 96) -> None:
    folder.mkdir(parents=True, exist_ok=False)
    for index in range(count):
        (folder / f"sidebar-item-{index:03d}.md").write_text(
            f"# Sidebar item {index:03d}\n\nfixture\n", encoding="utf-8"
        )


def evaluate_idle(metrics: dict[str, Any]) -> dict[str, Any]:
    score = metrics.get("boundary_score")
    divider = metrics.get("divider_columns")
    if not isinstance(score, (int, float)) or isinstance(score, bool) or score < 8:
        return step("idle_boundary", "blocked", "sidebar-main boundary を安定検出できない", metrics=metrics)
    if not isinstance(divider, int) or isinstance(divider, bool):
        return step("idle_boundary", "blocked", "divider width evidence が不正", metrics=metrics)
    if divider > 4:
        return step(
            "idle_boundary", "fail",
            "idle 時の sidebar-main 境界が太い",
            divider_columns=divider,
            boundary_score=score,
        )
    return step(
        "idle_boundary", "pass",
        divider_columns=divider,
        boundary_score=score,
        boundary_x=metrics.get("boundary_x"),
        boundary_normalized_x=metrics.get("boundary_normalized_x"),
    )


def evaluate_scrolling(diff: dict[str, Any]) -> dict[str, Any]:
    run = diff.get("max_changed_run")
    width = diff.get("max_changed_width")
    if not isinstance(run, int) or not isinstance(width, int):
        return step("scrolling_thumb", "blocked", "scroll thumb diff evidence が不正", diff=diff)
    if run < 18:
        return step(
            "scrolling_thumb", "fail",
            "sidebar wheel 直後に右端の一時 scrollbar thumb を確認できない",
            **diff,
        )
    if width < 2 or width > 10:
        return step(
            "scrolling_thumb", "fail",
            "scrollbar thumb の見た目幅が focused acceptance の細い範囲外",
            **diff,
        )
    return step("scrolling_thumb", "pass", **diff)


def evaluate_hidden(diff: dict[str, Any]) -> dict[str, Any]:
    run = diff.get("max_changed_run")
    width = diff.get("max_changed_width")
    if not isinstance(run, int) or not isinstance(width, int):
        return step("post_scroll_hide", "blocked", "post-scroll diff evidence が不正", diff=diff)
    if run >= 18 and width >= 2:
        return step(
            "post_scroll_hide", "fail",
            "scroll 終了後も sidebar scrollbar thumb/track が残っている",
            **diff,
        )
    return step("post_scroll_hide", "pass", **diff)


def evaluate_resize(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    b = before.get("boundary_x")
    a = after.get("boundary_x")
    divider = after.get("divider_columns")
    if not isinstance(b, int) or not isinstance(a, int) or not isinstance(divider, int):
        return step("resize", "blocked", "resize boundary evidence が不正", before=before, after=after)
    shift = a - b
    if shift < 18:
        return step(
            "resize", "fail",
            "細い境界の hit area を実 OS drag しても sidebar 幅が十分変化しない",
            boundary_shift=shift,
            before_x=b,
            after_x=a,
        )
    if divider > 4:
        return step(
            "resize", "fail",
            "resize 後の idle 境界が太い",
            boundary_shift=shift,
            divider_columns=divider,
        )
    return step(
        "resize", "pass",
        boundary_shift=shift,
        before_x=b,
        after_x=a,
        divider_columns=divider,
    )


def main() -> int:
    target_value = os.environ.get("HANE_SIDEBAR_GUI_TARGET_DIR")
    control_value = os.environ.get("HANE_SIDEBAR_GUI_CONTROL_DIR")
    expected_sha = os.environ.get("HANE_SIDEBAR_GUI_EXPECTED_SHA", "")
    if not target_value or not control_value or not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        print("target dir, control dir and full expected SHA are required", file=sys.stderr)
        return 3

    target_dir = Path(target_value).resolve()
    control_dir = Path(control_value).resolve()
    run_dir = Path(
        os.environ.get("HANE_SIDEBAR_GUI_RUN_DIR")
        or (Path(tempfile.gettempdir()) / "hane-sidebar-gui")
    ).resolve()
    request_id = os.environ.get("HANE_SIDEBAR_GUI_REQUEST_ID") or f"sidebar-{os.getpid()}"
    run_dir.mkdir(parents=True, exist_ok=True)
    result_path = run_dir / "result.json"

    gui = load_module(control_dir, "scripts/gui_validate.py", "sidebar_gui_validate")
    interaction = load_module(
        control_dir, "scripts/hosted_gui_interaction.py", "sidebar_hosted_gui_interaction"
    )
    env = gui.RealEnvironment()
    priority = gui.RESULT_PRIORITY
    steps: list[dict[str, Any]] = []
    target_info: dict[str, Any] = {}
    build_info: dict[str, Any] = {}
    started_at = env.clock.now_iso()
    helper_tmp = tempfile.TemporaryDirectory(prefix="hane-sidebar-helper-")
    holder: dict[str, Any] = {"process": None}

    try:
        env.acquire_execution()
        fixture_dir = run_dir / "work-folder"
        make_fixtures(fixture_dir)
        config = interaction.make_config(
            gui,
            workspace_dir=target_dir,
            scenario="sidebar-focused",
            expected_sha=expected_sha,
            request_id=request_id,
            generation="1",
            run_dir=run_dir,
            fixture_path=fixture_dir,
            features=["timing-probe"],
            extra_env={},
            startup_timeout=20.0,
            window_timeout=30.0,
        )

        preflight, target_info = gui.do_preflight(env, config)
        steps.append(preflight)
        binary_path = None
        helper = None
        helper_digest = ""
        if preflight["result"] == "pass":
            try:
                helper, helper_digest = compile_helper(
                    control_dir / "scripts" / "hosted_sidebar_gui.swift",
                    Path(helper_tmp.name),
                )
                steps.append(step("prepare_helper", "pass", sha256=helper_digest))
            except (OSError, subprocess.SubprocessError) as exc:
                steps.append(step("prepare_helper", "blocked", str(exc)))

            if helper is not None:
                build_step, binary_path, build_info = gui.do_build(
                    env, config, target_info["actual_sha"]
                )
                steps.append(build_step)

        if binary_path is None or helper is None:
            steps.append(skipped("sidebar_scenario", "preflight/helper/build が pass しなかった"))
        else:
            launch = gui.do_launch(env, config, binary_path, holder)
            steps.append(launch)
            if launch["result"] != "pass":
                steps.append(skipped("sidebar_scenario", "launch が pass しなかった"))
            else:
                process = holder["process"]
                window_step, _window_id = gui.do_window_discovery(env, config, process)
                steps.append(window_step)
                if window_step["result"] != "pass":
                    steps.append(skipped("sidebar_scenario", "window discovery が pass しなかった"))
                else:
                    pid = process.pid
                    time.sleep(0.9)
                    idle_path = run_dir / "sidebar-idle.png"
                    run_helper(helper, helper_digest, ["capture", str(pid), str(idle_path)])
                    idle = helper_json(helper, helper_digest, ["analyze", str(idle_path)])
                    steps.append(evaluate_idle(idle))

                    boundary_norm = idle.get("boundary_normalized_x")
                    boundary_x = idle.get("boundary_x")
                    if not isinstance(boundary_norm, (int, float)) or not isinstance(boundary_x, int):
                        steps.append(step("scrolling_thumb", "blocked", "idle boundary coordinate が不正"))
                        steps.append(skipped("post_scroll_hide", "idle boundary coordinate が不正"))
                        steps.append(skipped("resize", "idle boundary coordinate が不正"))
                    else:
                        wheel_x = max(0.08, min(float(boundary_norm) - 0.025, 0.45))
                        scrolling_path = run_dir / "sidebar-scrolling.png"
                        run_helper(
                            helper,
                            helper_digest,
                            [
                                "wheel-capture",
                                str(pid),
                                f"{wheel_x:.6f}",
                                "0.50",
                                "-320",
                                str(scrolling_path),
                            ],
                        )
                        scroll_diff = helper_json(
                            helper,
                            helper_digest,
                            [
                                "compare-strip",
                                str(idle_path),
                                str(scrolling_path),
                                str(boundary_x),
                            ],
                        )
                        steps.append(evaluate_scrolling(scroll_diff))

                        time.sleep(0.9)
                        hidden_path = run_dir / "sidebar-after-hide.png"
                        run_helper(helper, helper_digest, ["capture", str(pid), str(hidden_path)])
                        hidden_diff = helper_json(
                            helper,
                            helper_digest,
                            [
                                "compare-strip",
                                str(idle_path),
                                str(hidden_path),
                                str(boundary_x),
                            ],
                        )
                        steps.append(evaluate_hidden(hidden_diff))

                        resized_path = run_dir / "sidebar-resized.png"
                        run_helper(
                            helper,
                            helper_digest,
                            [
                                "drag-capture",
                                str(pid),
                                f"{float(boundary_norm):.6f}",
                                "0.50",
                                "0.045",
                                str(resized_path),
                            ],
                        )
                        resized = helper_json(
                            helper, helper_digest, ["analyze", str(resized_path)]
                        )
                        steps.append(evaluate_resize(idle, resized))

        if holder.get("process") is not None:
            steps.append(gui.do_cleanup(env, holder["process"]))

        overall = worst(steps, priority)
        reason = reason_for(steps)
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
            "steps": steps,
            "overall_result": overall,
            "overall_reason": reason,
        }
        label = {"pass": "PASS", "fail": "FAIL", "blocked": "BLOCKED"}.get(overall, "BLOCKED")
        result["summary"] = f"[{label}] {PROCEDURE_VERSION} — {reason}"
        result_path.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        (run_dir / "summary.md").write_text(result["summary"] + "\n", encoding="utf-8")
        print(result["summary"])
        return EXIT_PASS if overall == "pass" else EXIT_NONPASS
    except Exception as exc:
        fallback = {
            "schema_version": SCHEMA_VERSION,
            "procedure_version": PROCEDURE_VERSION,
            "verification_kind": VERIFICATION_KIND,
            "scope_note": SCOPE_NOTE,
            "request_id": request_id,
            "overall_result": "blocked",
            "overall_reason": str(exc),
            "steps": steps,
        }
        result_path.write_text(json.dumps(fallback, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        (run_dir / "summary.md").write_text(
            f"[BLOCKED] {PROCEDURE_VERSION} — {exc}\n", encoding="utf-8"
        )
        print(f"[BLOCKED] {PROCEDURE_VERSION} — {exc}", file=sys.stderr)
        return EXIT_NONPASS
    finally:
        helper_tmp.cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
