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
PROCEDURE_VERSION = "hosted-date-badge/3"
VERIFICATION_KIND = "sidebar_date_badge_focused"
SCOPE_NOTE = (
    "Issue #174 の sidebar date-badge 表示だけを検証する focused GUI evidence。"
    "hosted-gui-interaction/7 の包括的 GUI 検証合格を意味しない。"
)
EXIT_PASS = 0
EXIT_NONPASS = 1
# Sidebar rows are about 24 px apart in the default 760 px-tall hosted window,
# or ~0.032 in Vision-normalized coordinates. Keep this below half a row so a
# same-label badge on an adjacent row can never satisfy the same-row match.
SAME_ROW_CENTER_Y_TOLERANCE = 0.014


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


def group_candidates_by_observation(candidates: list[dict]) -> dict[int, list[dict]]:
    """Group Vision matches by the OCR observation (sidebar text line) they came from.

    `helper_find_all` can now return more than one match per observation: one
    per bounded Vision candidate (`candidate_rank`) whose own text happens to
    satisfy the search pattern. Grouping by `observation_index` recovers which
    matches describe the same physical line versus genuinely different rows.
    """
    groups: dict[int, list[dict]] = {}
    for candidate in candidates:
        groups.setdefault(candidate["observation_index"], []).append(candidate)
    for group in groups.values():
        group.sort(key=lambda item: item["candidate_rank"])
    return groups


def best_matches(candidates: list[dict]) -> list[dict]:
    """Reduce to Vision's most-confident (lowest `candidate_rank`) match per observation.

    Vision's own top-1 candidate for an observation can misread a glyph
    (Issue #187), in which case top-1 simply never appears here because its
    text does not satisfy the search pattern at all -- only a lower-ranked
    alternate's own text does. Picking the lowest available rank per
    observation recovers that alternate without ever preferring a
    less-confident alternate over a top-ranked one that is already present:
    if the most confident available match for an observation still fails a
    downstream exact/geometry check, this function does not search further
    alternates to find one that would conveniently pass (fail-closed).
    """
    return [group[0] for group in group_candidates_by_observation(candidates).values()]


def best_matches_satisfying(candidates: list[dict], predicate) -> list[dict]:
    """Reduce to the most-confident candidate per observation whose own line satisfies `predicate`.

    Unlike `best_matches`, this does not first fix the most-confident match
    to a looser search pattern (e.g. a filename prefix) and then test it
    against a stricter whole-line predicate. A less-confident bounded
    alternate can be the only one whose own `recognized_line` satisfies
    `predicate` exactly, while a more-confident alternate for the same
    observation only matched the looser pattern by misreading a different
    part of that same line -- e.g. the filename prefix reads correctly but
    the weekday glyph does not (Issue #188). Testing every bounded alternate
    against `predicate` before ranking keeps the observation's OCR
    hypothesis self-consistent, and, mirroring `best_matches`, never prefers
    a less-confident alternate once a more-confident one already satisfies
    `predicate` (fail-closed).
    """
    result = []
    for group in group_candidates_by_observation(candidates).values():
        match = next((candidate for candidate in group if predicate(candidate)), None)
        if match is not None:
            result.append(match)
    return result


def nearest_same_row(text_match: dict, badge_matches: list[dict]) -> dict | None:
    if not badge_matches:
        return None
    candidate = min(badge_matches, key=lambda item: abs(center_y(item) - center_y(text_match)))
    return (
        candidate
        if abs(center_y(candidate) - center_y(text_match)) <= SAME_ROW_CENTER_Y_TOLERANCE
        else None
    )


def normalized_ocr_text(value: str) -> str:
    return re.sub(r"\s+", " ", value).strip()


def recognized_line_matches_expected(match: dict, expected: str) -> bool:
    return normalized_ocr_text(match.get("recognized_line", "")) == normalized_ocr_text(expected)


def exact_label_matches(candidates: list[dict], expected: str) -> list[dict]:
    """Keep only candidates whose whole OCR line equals expected, fail-closed.

    `helper_find_all` locates candidates by substring search, so a badge
    observation like `2026/1/2(金)` can be returned for an expected label of
    `1/2(金)`. Only a full recognized-line match proves the badge itself, not
    a longer line that merely contains the label as a substring.

    Callers should pass every bounded alternate for the search (not a
    per-observation reduction like `best_matches`), so an exact-matching
    lower-confidence alternate is never discarded in favor of a
    higher-confidence alternate for the same observation that only matched
    the looser search pattern (Issue #188). `nearest_same_row` still performs
    the row disambiguation afterwards.
    """
    return [candidate for candidate in candidates if recognized_line_matches_expected(candidate, expected)]


def joined_row_candidate(full_line: str, badge_candidates: list[dict]) -> dict | None:
    """Find a badge observation that is literally the same OCR line as `full_line`.

    Vision sometimes joins a sidebar filename and its badge into a single
    `recognized_line` (e.g. `Alpha.md 本日`) instead of two observations. Such
    a badge candidate's own `recognized_line` equals the row's full line, not
    just the badge label, so it cannot be found by `exact_label_matches`.

    Callers should pass every bounded alternate for the badge search (not a
    per-observation reduction like `best_matches`), so the alternate whose
    own line truly equals `full_line` is never missed in favor of a
    higher-confidence alternate for the same observation that matched the
    badge-label search pattern without being the same whole line (Issue
    #188).
    """
    normalized_full = normalized_ocr_text(full_line)
    return next(
        (
            candidate
            for candidate in badge_candidates
            if normalized_ocr_text(candidate.get("recognized_line", "")) == normalized_full
        ),
        None,
    )


def joined_composition_is_exact(full_line: str, expected_display: str, expected_badge: str) -> bool:
    """Fail-closed check that a joined row is exactly `expected display + badge`.

    Only a single optional space (or no separator, for OCR spacing loss) is
    tolerated between the two parts. Any extra prefix/suffix, or a different
    badge label such as a longer date that merely contains the expected badge
    as a substring (e.g. `2026/1/2(金)` vs expected `1/2(金)`), is rejected.
    """
    normalized = normalized_ocr_text(full_line)
    display = normalized_ocr_text(expected_display)
    return normalized in (f"{display} {expected_badge}", f"{display}{expected_badge}")


def joined_composition_ends_with_badge(full_line: str, expected_badge: str) -> bool:
    return normalized_ocr_text(full_line).endswith(normalized_ocr_text(expected_badge))


def display_geometry_match(match: dict, *, use_full_line: bool, joined_row: bool = False) -> dict:
    """Return geometry representing the whole visible filename when needed.

    Normal short cases use an anchored regex whose match is already the whole
    display name (badge, if any, is excluded by the caller's lookahead
    pattern, so no clipping is needed even when the row is joined). The
    long-name case intentionally cannot know the exact truncation point, so
    a split-row prefix match is only an identity locator; the right-side/
    non-overlap oracle there must use Vision's full recognized-line box.

    When Vision instead joins that long-name row with its badge into one
    recognized_line, the full line box would span the badge itself. Clipping
    that box to the badge *under verification*'s own minX made the
    non-overlap oracle an unfalsifiable tautology: the badge always
    satisfies `badge.minX >= clipped_maxX` because the clip endpoint *is*
    that same badge's minX, regardless of whether it truly overlaps visible
    filename text (Issue #185 follow-up false-PASS). The long-name
    text_pattern is greedy over the visible filename words plus an optional
    trailing ellipsis, so on a joined row its own regex match box already
    covers the whole visible filename independently of the badge. Use that
    match box unmodified instead of deriving filename geometry from the
    badge under verification.
    """
    if not use_full_line or joined_row:
        return match
    line_box = match.get("line_bounding_box")
    required = {"minX", "maxX", "minY", "maxY"}
    if not isinstance(line_box, dict) or not required.issubset(line_box):
        raise ValueError("Vision evidence is missing the full recognized-line bounding box")
    return {**match, "bounding_box": dict(line_box)}


def badge_is_strictly_right(text_match: dict, badge_match: dict) -> bool:
    text_box = text_match["bounding_box"]
    badge_box = badge_match["bounding_box"]
    return float(badge_box["minX"]) >= float(text_box["maxX"])


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


def date_token_pattern(token: str) -> str:
    """OCR-tolerant pattern for a hyphenated filename date token."""
    parts = token.split("-")
    if len(parts) != 3 or not all(part.isascii() and part.isdecimal() for part in parts):
        raise ValueError(f"invalid hyphenated date token: {token!r}")
    return r"\s*-\s*".join(re.escape(part) for part in parts)


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


def make_fixtures(folder: Path, *, today: dt.date | None = None) -> tuple[list[str], list[dict]]:
    """Build sidebar fixtures whose badges cover all four `relative_label` shapes.

    Each non-`本日` fixture date is derived from `today` so that, regardless
    of which real day the hosted GUI run executes on, the five fixtures still
    exercise 本日 / same-month-different-day / same-year-different-month /
    previous-year once each.
    """
    folder.mkdir(parents=True, exist_ok=False)
    if today is None:
        today = dt.date.today()
    today_token = today.strftime("%Y-%m-%d")

    middle_day = 1 if today.day != 1 else 2
    date_in_middle = dt.date(today.year, today.month, middle_day)
    date_in_middle_token = date_in_middle.strftime("%Y-%m-%d")

    one_digit_month = 1 if today.month != 1 else 2
    one_digit = dt.date(today.year, one_digit_month, 2)
    one_digit_token = f"{one_digit.year}-{one_digit.month}-{one_digit.day}"

    date_at_end = dt.date(today.year - 1, today.month, 2)
    date_at_end_token = date_at_end.strftime("%Y-%m-%d")

    cases = [
        {
            "name": "date_at_start",
            "filename": f"{today_token}_Alpha.md",
            "text_pattern": r"^\s*Alpha\.md",
            "expected_display": "Alpha.md",
            "date_token": today_token,
            "badge_label": "本日",
        },
        {
            "name": "date_in_middle",
            "filename": f"Bravo_{date_in_middle_token}_Note.md",
            "text_pattern": r"^\s*Bravo\s+Note\.md",
            "expected_display": "Bravo Note.md",
            "date_token": date_in_middle_token,
            "badge_label": relative_label(date_in_middle, today),
        },
        {
            "name": "date_at_end",
            "filename": f"Charlie_{date_at_end_token}.md",
            "text_pattern": r"^\s*Charlie\.md",
            "expected_display": "Charlie.md",
            "date_token": date_at_end_token,
            "badge_label": relative_label(date_at_end, today),
        },
        {
            "name": "one_digit_month_day",
            "filename": f"{one_digit_token}_Delta.md",
            "text_pattern": r"^\s*Delta\.md",
            "expected_display": "Delta.md",
            "date_token": one_digit_token,
            "badge_label": relative_label(one_digit, today),
        },
        {
            "name": "long_name_keeps_badge_visible",
            "filename": (
                "This_Is_An_Extremely_Long_Sidebar_Filename_Designed_To_Force_"
                f"Truncation_{today_token}.md"
            ),
            # For a split row (filename alone on its own recognized_line),
            # this prefix only identifies the intended OCR observation and
            # the full `recognized_line` bbox is used for the strict
            # right-side/non-overlap check. For a row Vision joins with the
            # badge into one recognized_line, this regex match itself --
            # greedy over the visible filename words plus an optional
            # trailing ellipsis -- is used as independent filename geometry
            # instead, so the check never derives filename geometry from the
            # very badge being verified (Issue #185 follow-up false-PASS).
            "text_pattern": r"This(?:[_ ]?[A-Za-z]+)+(?:\s*(?:…|\.\.\.))?",
            "expected_display": None,
            "date_token": today_token,
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
                                date_cache: dict[str, list[dict]] = {}
                                for case in cases:
                                    try:
                                        # `best_matches` reduces Vision's bounded per-candidate
                                        # matches (Issue #187: top-1 alone can misread a tiny
                                        # sidebar glyph) to the single most-confident match per
                                        # OCR observation. The `_all` lists are kept as read-only
                                        # evidence for fail-closed diagnostics, and are also
                                        # re-searched directly below (via
                                        # `best_matches_satisfying`) whenever a stricter
                                        # whole-line predicate must consider every bounded
                                        # alternate rather than only the one alternate
                                        # `best_matches` picked for a looser pattern search
                                        # (Issue #188).
                                        texts_all = helper_find_all(helper, helper_digest, screenshot, case["text_pattern"])
                                        texts = best_matches(texts_all)
                                        badges_all = badge_cache.setdefault(
                                            case["badge_label"],
                                            helper_find_all(
                                                helper, helper_digest, screenshot, re.escape(case["badge_label"])
                                            ),
                                        )
                                        token_pattern = date_token_pattern(case["date_token"])
                                        date_matches_all = date_cache.setdefault(
                                            case["date_token"],
                                            helper_find_all(helper, helper_digest, screenshot, token_pattern),
                                        )
                                        date_matches = best_matches(date_matches_all)
                                        if not texts:
                                            scenario_steps.append(step(
                                                case["name"],
                                                "fail",
                                                "省略後の表示ファイル名を screenshot OCR で確認できない",
                                                text_candidates=texts_all,
                                            ))
                                            continue
                                        text_match = texts[0]
                                        full_line = text_match.get("recognized_line", "")
                                        expected_display = case.get("expected_display")

                                        # Vision may keep the filename and badge as separate
                                        # observations (split row) or join them into one
                                        # `recognized_line` such as `Alpha.md 本日` (joined
                                        # row). Which bounded alternate is the correct whole-line
                                        # reading of that row is decided per predicate directly
                                        # against each candidate's own `recognized_line`, never
                                        # by mixing `text_match`'s own (possibly wrong-rank) line
                                        # with a candidate an independent badge search happened
                                        # to pick. `text_match`/`full_line` are only replaced by
                                        # the specific alternate that itself satisfies the
                                        # accepted row kind.
                                        if expected_display is not None:
                                            split_matches = best_matches_satisfying(
                                                texts_all,
                                                lambda candidate: recognized_line_matches_expected(candidate, expected_display),
                                            )
                                            split_candidate = split_matches[0] if split_matches else None
                                            joined_line_matches = best_matches_satisfying(
                                                texts_all,
                                                lambda candidate: joined_composition_is_exact(
                                                    candidate.get("recognized_line", ""), expected_display, case["badge_label"]
                                                ),
                                            )
                                            joined_line_candidate = joined_line_matches[0] if joined_line_matches else None
                                            if split_candidate is not None:
                                                is_split_row, is_joined_row = True, False
                                                text_match = split_candidate
                                            elif joined_line_candidate is not None:
                                                is_split_row, is_joined_row = False, True
                                                text_match = joined_line_candidate
                                            else:
                                                is_split_row = is_joined_row = False
                                            if not is_split_row and not is_joined_row:
                                                scenario_steps.append(step(
                                                    case["name"],
                                                    "fail",
                                                    "sidebar の表示名全体が期待値と一致しない",
                                                    filename=case["filename"],
                                                    expected_display=expected_display,
                                                    recognized_line=full_line,
                                                    text_match=text_match,
                                                    text_candidates=texts_all,
                                                    badge_candidates=badges_all,
                                                    screenshot=str(screenshot),
                                                ))
                                                continue
                                        else:
                                            joined_line_matches = best_matches_satisfying(
                                                texts_all,
                                                lambda candidate: joined_composition_ends_with_badge(
                                                    candidate.get("recognized_line", ""), case["badge_label"]
                                                ),
                                            )
                                            joined_line_candidate = joined_line_matches[0] if joined_line_matches else None
                                            is_joined_row = joined_line_candidate is not None
                                            if is_joined_row:
                                                text_match = joined_line_candidate
                                        full_line = text_match.get("recognized_line", "")

                                        # `joined_candidate` is the badge observation whose own
                                        # OCR line is literally that same accepted row, searched
                                        # across every bounded alternate in `badges_all` (not a
                                        # pre-reduced rank-0-first subset) so the badge substring
                                        # geometry comes from the same OCR hypothesis as
                                        # `text_match`, used only for its bounding box.
                                        joined_candidate = joined_row_candidate(full_line, badges_all) if is_joined_row else None

                                        display_match = display_geometry_match(
                                            text_match,
                                            use_full_line=expected_display is None,
                                            joined_row=is_joined_row,
                                        )
                                        date_on_row = nearest_same_row(display_match, date_matches)
                                        date_in_line = re.search(token_pattern, full_line) is not None
                                        if date_on_row is not None or date_in_line:
                                            scenario_steps.append(step(
                                                case["name"],
                                                "fail",
                                                "元ファイル名の日付 token が sidebar の表示名から除去されていない",
                                                filename=case["filename"],
                                                date_token=case["date_token"],
                                                text_match=text_match,
                                                display_geometry=display_match["bounding_box"],
                                                date_on_row=date_on_row,
                                                date_candidates=date_matches_all,
                                                screenshot=str(screenshot),
                                            ))
                                            continue
                                        badge_candidates = (
                                            ([joined_candidate] if joined_candidate is not None else [])
                                            if is_joined_row
                                            else exact_label_matches(badges_all, case["badge_label"])
                                        )
                                        badge_match = nearest_same_row(display_match, badge_candidates)
                                        if badge_match is None:
                                            scenario_steps.append(step(
                                                case["name"],
                                                "fail",
                                                "同じ sidebar row の日付バッジを確認できない",
                                                badge_candidates=badges_all,
                                            ))
                                            continue
                                        right_side = badge_is_strictly_right(display_match, badge_match)
                                        scenario_steps.append(step(
                                            case["name"],
                                            "pass" if right_side else "fail",
                                            None if right_side else "日付バッジが省略後の表示ファイル名全体と重ならず右側にない",
                                            filename=case["filename"],
                                            expected_badge=case["badge_label"],
                                            date_token=case["date_token"],
                                            text_match=text_match,
                                            text_candidates=texts_all,
                                            display_geometry=display_match["bounding_box"],
                                            badge_match=badge_match,
                                            badge_candidates=badges_all,
                                            date_matches=date_matches,
                                            date_candidates=date_matches_all,
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
        result["summary"] = f"[{label}] {PROCEDURE_VERSION} — {reason}"
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
        print(f"[BLOCKED] {PROCEDURE_VERSION} — {exc}")
        return EXIT_NONPASS
    finally:
        env.release_execution()
        helper_tmp.cleanup()


if __name__ == "__main__":
    sys.exit(main())