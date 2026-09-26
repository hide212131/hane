#!/usr/bin/env python3
"""Trusted focused GUI validation for the file-tab hover path/copy affordance (Issue #354).

One fixed fixture, one direct-open Hane launch, one real-OS theme cycle
(System -> Light -> Dark) with a screenshot-pixel judgement of the selected
tab's background per theme, one real-OS hover over the tab label to reveal
its HoverCard, a pixel-contrast probe that locates the copy icon without
trusting the target's own rendering as an authority, one real-OS click on
that icon, and an exact clipboard comparison against the fixture's absolute
path. This procedure does not repeat comprehensive IME, list, sidebar,
code-block, or full-regression scenarios.
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

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "hosted-file-tabs/1"
VERIFICATION_KIND = "file_tabs_focused"
SCOPE_NOTE = (
    "Issue #354 の file tab hover path / copy icon に限定した focused GUI evidence。"
    "固定 fixture の direct-open、System/Light/Dark の実 OS クリックによる選択タブ背景の"
    "screenshot pixel 判定、タブラベルへの実 OS hover による HoverCard の絶対 path 表示、"
    "copy icon の pixel/ink 判定と実 OS クリック、clipboard の完全一致確認だけを行う。"
    "他の focused/comprehensive GUI 検証の合格を意味しない。"
)
EXIT_PASS = 0
EXIT_NONPASS = 1

FIXTURE_FILENAME = "file-tabs-focus.md"
FIXTURE_CONTENT = (
    "# file tabs focused fixture\n"
    "\n"
    "Hover the tab to reveal its absolute path.\n"
)

# The footer theme control renders exactly `Theme {:?}` of ThemePreference
# (session::store::ThemePreference), so the visible label is always one of
# these three literal words.
THEME_PATTERN = r"Theme\s+(System|Light|Dark)"

# A selected-tab background at or above this level on every channel is
# indistinguishable from white regardless of theme; dark theme must never
# report one (Issue #354).
NEAR_WHITE_THRESHOLD = 235
# Minimum per-channel delta required to call two sampled backgrounds visibly
# different; anti-aliasing/compression noise alone should never exceed this.
MIN_BACKGROUND_DELTA = 24
# Fraction of the fixture's absolute path (whitespace stripped) that must be
# covered by OCR fragments matched against it before the HoverCard is
# considered to show the path. Less than 1.0 tolerates a stray OCR miss on a
# character or two without accepting an unrelated screen as a match.
PATH_OCR_COVERAGE_MIN = 0.9
# Plausible pixel-size bounds for the copy icon glyph, used only to reject a
# clearly-degenerate probe result (e.g. a single stray pixel or a region
# spanning most of the search band) rather than to assert an exact size.
ICON_MIN_PIXELS = 3
ICON_MAX_PIXELS = 60


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


def step(name: str, result: str, reason: str | None = None, **detail) -> dict:
    return {"name": name, "result": result, "reason": reason, **detail}


def skipped(name: str, reason: str) -> dict:
    return step(name, "skipped", reason)


def reason_for(steps: list[dict]) -> str:
    reasons = [
        item.get("reason")
        for item in steps
        if item.get("result") not in ("pass", "skipped") and item.get("reason")
    ]
    return "; ".join(reasons) if reasons else "Issue #354 focused file-tabs GUI checks passed"


def overall_result(steps: list[dict], priority: dict[str, int]) -> str:
    values = [item["result"] for item in steps if item.get("result") in priority]
    return min(values, key=lambda value: priority[value]) if values else "blocked"


def env_float(name: str, default: float) -> float:
    value = os.environ.get(name)
    if not value:
        return default
    parsed = float(value)
    if parsed != parsed or parsed <= 0 or parsed == float("inf"):
        raise ValueError(f"{name} must be positive and finite")
    return parsed


def parse_hex_color(value: object) -> tuple[int, int, int]:
    if not isinstance(value, str) or not re.fullmatch(r"#[0-9a-fA-F]{6}", value):
        raise ValueError(f"invalid hex color: {value!r}")
    return (int(value[1:3], 16), int(value[3:5], 16), int(value[5:7], 16))


def is_near_white(value: str, threshold: int = NEAR_WHITE_THRESHOLD) -> bool:
    r, g, b = parse_hex_color(value)
    return r >= threshold and g >= threshold and b >= threshold


def colors_differ(a: str, b: str, min_delta: int = MIN_BACKGROUND_DELTA) -> bool:
    ar, ag, ab = parse_hex_color(a)
    br, bg, bb = parse_hex_color(b)
    return max(abs(ar - br), abs(ag - bg), abs(ab - bb)) >= min_delta


def theme_label_from_text(text: str) -> str | None:
    match = re.search(THEME_PATTERN, text)
    return match.group(1).lower() if match else None


def strip_whitespace(text: str) -> str:
    return re.sub(r"\s+", "", text)


def path_fragment_coverage(fragments: list[str], expected_path: str) -> float:
    expected = strip_whitespace(expected_path)
    if not expected:
        return 0.0
    covered = sum(len(strip_whitespace(fragment)) for fragment in fragments)
    return min(1.0, covered / len(expected))


def icon_probe_is_plausible(probe: dict) -> tuple[bool, str | None]:
    rect = probe.get("icon_pixel_rect")
    center = probe.get("icon_center_fraction")
    if not isinstance(rect, dict) or not isinstance(center, dict):
        return False, "copy icon probe の JSON 形式が不正"
    try:
        width = float(rect["x1"]) - float(rect["x0"])
        height = float(rect["y1"]) - float(rect["y0"])
    except (KeyError, TypeError, ValueError):
        return False, "copy icon probe の pixel rect が不正"
    if not (ICON_MIN_PIXELS <= width <= ICON_MAX_PIXELS and ICON_MIN_PIXELS <= height <= ICON_MAX_PIXELS):
        return False, f"copy icon の推定サイズが妥当な範囲外: width={width} height={height}"
    fx, fy = center.get("x"), center.get("y_from_top")
    if not (
        isinstance(fx, (int, float)) and not isinstance(fx, bool)
        and isinstance(fy, (int, float)) and not isinstance(fy, bool)
        and 0.0 < fx < 1.0 and 0.0 < fy < 1.0
    ):
        return False, f"copy icon の中心座標が画面範囲外: {center!r}"
    contrast_run = probe.get("contrast_run", 0)
    if not (isinstance(contrast_run, (int, float)) and not isinstance(contrast_run, bool) and contrast_run > 0):
        return False, "copy icon が背景と異なる pixel を持つ evidence がない"
    return True, None


def helper_json(interaction, helper, args: list[str], timeout: float) -> dict:
    ok, out, err = interaction.run_helper(helper, args, timeout)
    if not ok:
        raise RuntimeError(err or "helper failed")
    try:
        payload = json.loads(out)
    except json.JSONDecodeError as exc:
        raise ValueError(f"helper が不正な JSON を返した: {out!r}") from exc
    if not isinstance(payload, dict):
        raise ValueError("helper JSON はオブジェクトである必要がある")
    return payload


def run_focused_scenario(
    gui_validate, interaction, env, target_dir: Path, helper, run_dir: Path,
    binary_path: Path, expected_sha: str, request_id: str,
    startup_timeout: float, window_timeout: float, helper_timeout: float,
    priority: dict[str, int],
) -> dict:
    scenario_dir = run_dir / "file_tabs_focused"
    scenario_dir.mkdir(parents=True, exist_ok=True)
    fixture = (scenario_dir / FIXTURE_FILENAME).resolve()
    fixture.write_text(FIXTURE_CONTENT, encoding="utf-8")
    expected_absolute_path = str(fixture)
    config = interaction.make_config(
        gui_validate,
        workspace_dir=target_dir,
        scenario="file-tabs-focused",
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
    evidence: dict = {"fixture_path": expected_absolute_path}
    holder = {"process": None}
    pending = (
        "initial_theme_is_system", "switch_theme_to_light", "switch_theme_to_dark",
        "dark_selected_tab_background_rejects_near_white",
        "light_dark_selected_tab_background_differs",
        "hover_shows_hover_card_path", "copy_icon_visible_pixel_contrast",
        "click_copy_icon", "clipboard_matches_absolute_path",
    )
    try:
        session_steps, window_id = interaction.open_session(
            gui_validate, env, config, binary_path, holder, "initial", helper, helper_timeout
        )
        steps.extend(session_steps)
        pid = interaction.current_pid(holder)
        if pid is None or window_id is None:
            for name in pending:
                steps.append(skipped(name, "launch/window discovery が pass しなかった"))
        else:
            initial_image = scenario_dir / "initial.png"
            light_image: Path | None = None
            dark_image: Path | None = None
            hover_image: Path | None = None
            light_background: str | None = None
            dark_background: str | None = None
            hover_ok = False
            envelope: dict | None = None
            icon_probe: dict | None = None
            clicked = False

            try:
                initial_text = helper_json(interaction, helper, ["find-text", str(initial_image), THEME_PATTERN], helper_timeout)
                label = theme_label_from_text(initial_text.get("matched_text", ""))
                steps.append(step(
                    "initial_theme_is_system", "pass" if label == "system" else "fail",
                    None if label == "system" else f"起動直後の Theme 表示が System ではない: {label!r}",
                    matched_text=initial_text.get("matched_text"),
                ))
            except (RuntimeError, ValueError) as exc:
                steps.append(step("initial_theme_is_system", "blocked", str(exc)))

            try:
                click_evidence = helper_json(
                    interaction, helper, ["click-text", str(pid), str(initial_image), THEME_PATTERN], helper_timeout
                )
                light_image = scenario_dir / "theme-light.png"
                steps.append(interaction.capture_named(gui_validate, env, config, window_id, scenario_dir, "theme-light"))
                light_text = helper_json(interaction, helper, ["find-text", str(light_image), THEME_PATTERN], helper_timeout)
                light_label = theme_label_from_text(light_text.get("matched_text", ""))
                light_tab = helper_json(
                    interaction, helper, ["tab-background", str(light_image), re.escape(FIXTURE_FILENAME)], helper_timeout
                )
                light_background = light_tab.get("background_color")
                steps.append(step(
                    "switch_theme_to_light", "pass" if light_label == "light" else "fail",
                    None if light_label == "light" else f"クリック後の Theme 表示が Light ではない: {light_label!r}",
                    click_event=click_evidence, matched_text=light_text.get("matched_text"),
                    selected_tab_background=light_background,
                ))
            except (RuntimeError, ValueError) as exc:
                steps.append(step("switch_theme_to_light", "blocked", str(exc)))

            if light_background is not None and light_image is not None:
                try:
                    click_evidence = helper_json(
                        interaction, helper, ["click-text", str(pid), str(light_image), THEME_PATTERN], helper_timeout
                    )
                    dark_image = scenario_dir / "theme-dark.png"
                    steps.append(interaction.capture_named(gui_validate, env, config, window_id, scenario_dir, "theme-dark"))
                    dark_text = helper_json(interaction, helper, ["find-text", str(dark_image), THEME_PATTERN], helper_timeout)
                    dark_label = theme_label_from_text(dark_text.get("matched_text", ""))
                    dark_tab = helper_json(
                        interaction, helper, ["tab-background", str(dark_image), re.escape(FIXTURE_FILENAME)], helper_timeout
                    )
                    dark_background = dark_tab.get("background_color")
                    steps.append(step(
                        "switch_theme_to_dark", "pass" if dark_label == "dark" else "fail",
                        None if dark_label == "dark" else f"クリック後の Theme 表示が Dark ではない: {dark_label!r}",
                        click_event=click_evidence, matched_text=dark_text.get("matched_text"),
                        selected_tab_background=dark_background,
                    ))
                except (RuntimeError, ValueError) as exc:
                    steps.append(step("switch_theme_to_dark", "blocked", str(exc)))
            else:
                steps.append(skipped("switch_theme_to_dark", "Light への切替が確認できなかった"))

            if dark_background is not None:
                try:
                    near_white = is_near_white(dark_background)
                    steps.append(step(
                        "dark_selected_tab_background_rejects_near_white",
                        "fail" if near_white else "pass",
                        f"dark theme の選択タブ背景が near-white のまま: {dark_background}" if near_white else None,
                        background_color=dark_background,
                    ))
                except ValueError as exc:
                    steps.append(step("dark_selected_tab_background_rejects_near_white", "blocked", str(exc)))
            else:
                steps.append(skipped(
                    "dark_selected_tab_background_rejects_near_white", "dark theme の背景を取得できなかった"
                ))

            if light_background is not None and dark_background is not None:
                try:
                    differ = colors_differ(light_background, dark_background)
                    steps.append(step(
                        "light_dark_selected_tab_background_differs",
                        "pass" if differ else "fail",
                        None if differ else (
                            "light/dark の選択タブ背景が区別できない: "
                            f"light={light_background} dark={dark_background}"
                        ),
                        light_background=light_background, dark_background=dark_background,
                    ))
                except ValueError as exc:
                    steps.append(step("light_dark_selected_tab_background_differs", "blocked", str(exc)))
            else:
                steps.append(skipped(
                    "light_dark_selected_tab_background_differs", "light/dark いずれかの背景を取得できなかった"
                ))

            evidence["theme_backgrounds"] = {"light": light_background, "dark": dark_background}

            if dark_background is not None and dark_image is not None:
                try:
                    hover_evidence = helper_json(
                        interaction, helper,
                        ["hover-text", str(pid), str(dark_image), re.escape(FIXTURE_FILENAME)], helper_timeout
                    )
                    time.sleep(0.6)
                    hover_image = scenario_dir / "hover-path.png"
                    steps.append(interaction.capture_named(gui_validate, env, config, window_id, scenario_dir, "hover-path"))
                    fragments_payload = helper_json(
                        interaction, helper,
                        ["find-path-fragments", str(hover_image), strip_whitespace(expected_absolute_path)],
                        helper_timeout,
                    )
                    fragments = [str(item.get("text", "")) for item in fragments_payload.get("fragments", [])]
                    coverage = path_fragment_coverage(fragments, expected_absolute_path)
                    hover_ok = coverage >= PATH_OCR_COVERAGE_MIN
                    envelope = fragments_payload.get("envelope_pixel_rect")
                    steps.append(step(
                        "hover_shows_hover_card_path", "pass" if hover_ok else "fail",
                        None if hover_ok else f"HoverCard に絶対 path の表示を確認できない (coverage={coverage:.2f})",
                        hover_event=hover_evidence, expected_absolute_path=expected_absolute_path,
                        ocr_fragments=fragments, coverage=coverage,
                    ))
                except (RuntimeError, ValueError) as exc:
                    steps.append(step("hover_shows_hover_card_path", "blocked", str(exc)))
            else:
                steps.append(skipped("hover_shows_hover_card_path", "dark theme への切替が確認できなかった"))

            if hover_ok and envelope and hover_image is not None:
                try:
                    icon_probe = helper_json(
                        interaction, helper,
                        ["copy-icon-probe", str(hover_image),
                         str(envelope.get("x0")), str(envelope.get("y0")),
                         str(envelope.get("x1")), str(envelope.get("y1"))],
                        helper_timeout,
                    )
                    plausible, reason = icon_probe_is_plausible(icon_probe)
                    steps.append(step(
                        "copy_icon_visible_pixel_contrast", "pass" if plausible else "fail", reason,
                        probe=icon_probe,
                    ))
                    if not plausible:
                        icon_probe = None
                except (RuntimeError, ValueError) as exc:
                    steps.append(step("copy_icon_visible_pixel_contrast", "blocked", str(exc)))
                    icon_probe = None
            else:
                steps.append(skipped("copy_icon_visible_pixel_contrast", "HoverCard の絶対 path を確認できなかった"))

            if icon_probe is not None:
                try:
                    center = icon_probe["icon_center_fraction"]
                    click_evidence = helper_json(
                        interaction, helper,
                        ["click-fraction", str(pid), str(center["x"]), str(center["y_from_top"])],
                        helper_timeout,
                    )
                    steps.append(step("click_copy_icon", "pass", click_event=click_evidence))
                    clicked = True
                except (RuntimeError, ValueError, KeyError) as exc:
                    steps.append(step("click_copy_icon", "blocked", str(exc)))
            else:
                steps.append(skipped("click_copy_icon", "copy icon の位置を確認できなかった"))

            if clicked:
                ok, clipboard_text, err = interaction.run_helper(helper, ["read-clipboard"], helper_timeout)
                if not ok:
                    steps.append(step("clipboard_matches_absolute_path", "blocked", err))
                else:
                    matched = clipboard_text == expected_absolute_path
                    steps.append(step(
                        "clipboard_matches_absolute_path", "pass" if matched else "fail",
                        None if matched else "コピーされた clipboard が fixture の絶対 path と完全一致しない",
                        expected=expected_absolute_path, actual=clipboard_text,
                    ))
            else:
                steps.append(skipped("clipboard_matches_absolute_path", "copy icon のクリックが確認できなかった"))
    finally:
        if holder.get("process") is not None:
            steps.append(interaction.close_session(gui_validate, env, holder))

    fixture_untouched = fixture.exists() and fixture.read_text(encoding="utf-8") == FIXTURE_CONTENT
    steps.append(step(
        "fixture_unchanged", "pass" if fixture_untouched else "fail",
        None if fixture_untouched else "GUI 操作で fixture が変更された",
    ))

    return {
        "name": "file_tabs_focused",
        "steps": steps,
        "result": overall_result(steps, priority),
        "reason": reason_for(steps),
        "evidence": evidence,
    }


def main() -> int:
    target_value = os.environ.get("HANE_FILE_TABS_GUI_TARGET_DIR")
    control_value = os.environ.get("HANE_FILE_TABS_GUI_CONTROL_DIR")
    expected_sha = os.environ.get("HANE_FILE_TABS_GUI_EXPECTED_SHA", "")
    if not target_value or not control_value or not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        print("target dir, control dir and full expected SHA are required", file=sys.stderr)
        return 3

    target_dir = Path(target_value).resolve()
    control_dir = Path(control_value).resolve()
    run_dir = Path(
        os.environ.get("HANE_FILE_TABS_GUI_RUN_DIR")
        or (Path(tempfile.gettempdir()) / "hane-file-tabs-gui")
    ).resolve()
    request_id = os.environ.get("HANE_FILE_TABS_GUI_REQUEST_ID") or f"file-tabs-{os.getpid()}"
    startup_timeout = env_float("HANE_FILE_TABS_GUI_STARTUP_TIMEOUT_SECS", 15.0)
    window_timeout = env_float("HANE_FILE_TABS_GUI_WINDOW_TIMEOUT_SECS", 30.0)
    helper_timeout = env_float("HANE_FILE_TABS_GUI_HELPER_TIMEOUT_SECS", 20.0)
    run_dir.mkdir(parents=True, exist_ok=True)
    result_path = run_dir / "result.json"

    gui_validate = load_module(control_dir, "scripts/gui_validate.py", "file_tabs_gui_validate")
    interaction = load_module(control_dir, "scripts/hosted_gui_interaction.py", "file_tabs_gui_interaction")
    env = gui_validate.RealEnvironment()
    priority = gui_validate.RESULT_PRIORITY
    top_steps: list[dict] = []
    scenarios: list[dict] = []
    target_info: dict = {}
    build_info: dict = {}
    started_at = env.clock.now_iso()
    helper_tmp = tempfile.TemporaryDirectory(prefix="hane-file-tabs-helper-")

    try:
        env.acquire_execution()
        config = interaction.make_config(
            gui_validate,
            workspace_dir=target_dir,
            scenario="file-tabs-preflight",
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
                    control_dir / "scripts" / "hosted_file_tabs_gui.swift",
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
                startup_timeout, window_timeout, helper_timeout, priority,
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
        reason = "; ".join(reasons) or "Issue #354 focused file-tabs GUI checks passed"
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
        label = {"pass": "PASS", "fail": "FAIL", "blocked": "BLOCKED"}.get(overall, "BLOCKED")
        result["summary"] = f"[{label}] {PROCEDURE_VERSION} — {reason}"
        result_path.write_text(
            json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
        (run_dir / "summary.md").write_text(result["summary"] + "\n", encoding="utf-8")
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
            json.dumps(fallback, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
        print(f"[BLOCKED] {PROCEDURE_VERSION} — {exc}")
        return EXIT_NONPASS
    finally:
        env.release_execution()
        helper_tmp.cleanup()


if __name__ == "__main__":
    sys.exit(main())
