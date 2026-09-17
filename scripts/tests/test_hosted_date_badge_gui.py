import datetime as dt
import importlib.util
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "hosted_date_badge_gui.py"
spec = importlib.util.spec_from_file_location("hosted_date_badge_gui", SCRIPT)
assert spec is not None and spec.loader is not None
mod = importlib.util.module_from_spec(spec)
sys.modules["hosted_date_badge_gui"] = mod
spec.loader.exec_module(mod)


class HostedDateBadgeGuiTests(unittest.TestCase):
    def test_procedure_identity_is_focused(self):
        self.assertEqual(mod.PROCEDURE_VERSION, "hosted-date-badge/2")
        self.assertEqual(mod.VERIFICATION_KIND, "sidebar_date_badge_focused")

    def test_relative_label_shapes(self):
        today = dt.date(2026, 9, 17)
        self.assertEqual(mod.relative_label(today, today), "本日")
        self.assertEqual(mod.relative_label(dt.date(2026, 9, 1), today), "1日(火)")
        self.assertEqual(mod.relative_label(dt.date(2026, 10, 3), today), "10/3(土)")
        self.assertEqual(mod.relative_label(dt.date(2025, 10, 3), today), "2025/10/3(金)")

    def test_date_token_pattern_tolerates_ocr_spacing(self):
        pattern = mod.date_token_pattern("2026-9-1")
        self.assertIsNotNone(re.search(pattern, "2026-9-1"))
        self.assertIsNotNone(re.search(pattern, "2026 - 9 - 1"))
        self.assertIsNone(re.search(pattern, "2026/9/1"))
        with self.assertRaises(ValueError):
            mod.date_token_pattern("20260901")

    def test_nearest_same_row_rejects_adjacent_row_badge(self):
        text = {"bounding_box": {"minX": 0.10, "maxX": 0.20, "minY": 0.50, "maxY": 0.52}}
        same_row = {"bounding_box": {"minX": 0.25, "maxX": 0.30, "minY": 0.495, "maxY": 0.515}}
        adjacent_row = {"bounding_box": {"minX": 0.25, "maxX": 0.30, "minY": 0.469, "maxY": 0.489}}
        self.assertIs(mod.nearest_same_row(text, [adjacent_row, same_row]), same_row)
        self.assertIsNone(mod.nearest_same_row(text, [adjacent_row]))
        self.assertLess(mod.SAME_ROW_CENTER_Y_TOLERANCE, 0.016)

    def test_recognized_line_must_be_the_complete_display_name(self):
        exact = {"recognized_line": " Alpha.md "}
        separator_left = {"recognized_line": "_Alpha.md"}
        badge_joined = {"recognized_line": "Alpha.md 本日"}
        self.assertTrue(mod.recognized_line_matches_expected(exact, "Alpha.md"))
        self.assertFalse(mod.recognized_line_matches_expected(separator_left, "Alpha.md"))
        self.assertFalse(mod.recognized_line_matches_expected(badge_joined, "Alpha.md"))

    def test_exact_label_matches_rejects_longer_recognized_line(self):
        expected = "1/2(金)"
        exact = {"recognized_line": "1/2(金)"}
        longer_prefix = {"recognized_line": "2026/1/2(金)"}
        suffix = {"recognized_line": "1/2(金)本日"}
        prefix = {"recognized_line": "本日1/2(金)"}
        filtered = mod.exact_label_matches([longer_prefix, suffix, prefix, exact], expected)
        self.assertEqual(filtered, [exact])

    def test_joined_row_candidate_finds_badge_on_the_same_recognized_line(self):
        same_line = {"recognized_line": "Alpha.md 本日", "bounding_box": {}}
        other_line = {"recognized_line": "本日", "bounding_box": {}}
        self.assertIs(mod.joined_row_candidate("Alpha.md 本日", [other_line, same_line]), same_line)
        self.assertIsNone(mod.joined_row_candidate("Alpha.md", [other_line]))

    def test_joined_composition_is_exact_accepts_only_display_plus_badge(self):
        self.assertTrue(mod.joined_composition_is_exact("Alpha.md 本日", "Alpha.md", "本日"))
        # OCR sometimes loses the separating space; still an exact composition.
        self.assertTrue(mod.joined_composition_is_exact("Alpha.md本日", "Alpha.md", "本日"))
        # A longer badge that merely contains the expected badge as a
        # substring must not be accepted (Issue #185 regression).
        self.assertFalse(mod.joined_composition_is_exact("Delta.md 2026/1/2(金)", "Delta.md", "1/2(金)"))
        self.assertFalse(mod.joined_composition_is_exact("Alpha.md 本日 extra", "Alpha.md", "本日"))
        self.assertFalse(mod.joined_composition_is_exact("prefix Alpha.md 本日", "Alpha.md", "本日"))

    def test_joined_composition_ends_with_badge(self):
        self.assertTrue(mod.joined_composition_ends_with_badge("This Is Long…本日", "本日"))
        self.assertFalse(mod.joined_composition_ends_with_badge("本日 This Is Long…", "本日"))
        self.assertFalse(mod.joined_composition_ends_with_badge("This Is Long…本日 extra", "本日"))

    def test_joined_ocr_fixture_matches_expected_acceptance(self):
        # Regression input shapes captured from PR #175 focused GUI run
        # #35250368931 (Issue #185): Vision may split the filename/badge into
        # separate observations or join them into one recognized_line.
        fixture_path = Path(__file__).resolve().parent / "fixtures" / "date_badge_joined_ocr_cases.json"
        payload = json.loads(fixture_path.read_text(encoding="utf-8"))
        for case in payload["cases"]:
            with self.subTest(case=case["name"]):
                text_match = {"recognized_line": case["text_recognized_line"]}
                badge_match = {"recognized_line": case["badge_recognized_line"]}
                is_split = mod.recognized_line_matches_expected(text_match, case["expected_display"])
                joined_candidate = mod.joined_row_candidate(case["text_recognized_line"], [badge_match])
                is_joined = joined_candidate is not None and mod.joined_composition_is_exact(
                    case["text_recognized_line"], case["expected_display"], case["expected_badge"]
                )
                self.assertEqual(is_split or is_joined, case["accepted"])

    def test_short_name_text_pattern_finds_row_before_composition_decides(self):
        # Issue #185 follow-up: the fixture-only test above never exercises
        # the actual `text_pattern` regex used by `helper_find_all` to locate
        # the OCR candidate in the first place. A too-strict pattern (e.g.
        # requiring `.md` to be followed by whitespace/end) silently drops
        # the candidate before the whole-line composition check ever runs,
        # so `Alpha.md本日` (OCR lost the separating space) would never reach
        # `joined_composition_is_exact` at all. This test runs the real
        # `text_pattern` from `make_fixtures` together with the real
        # composition functions against full OCR-recognized lines.
        with tempfile.TemporaryDirectory() as tmp:
            _, cases = mod.make_fixtures(Path(tmp) / "work-folder", today=dt.date(2026, 9, 17))
        pattern_by_display = {
            case["expected_display"]: case["text_pattern"]
            for case in cases
            if case["expected_display"] is not None
        }

        def resolve(display: str, badge: str, full_line: str) -> bool:
            pattern = pattern_by_display[display]
            if re.search(pattern, full_line) is None:
                return False
            badge_candidates = [{"recognized_line": full_line}] if badge in full_line else []
            is_split = mod.recognized_line_matches_expected({"recognized_line": full_line}, display)
            joined_candidate = mod.joined_row_candidate(full_line, badge_candidates)
            is_joined = joined_candidate is not None and mod.joined_composition_is_exact(
                full_line, display, badge
            )
            return is_split or is_joined

        self.assertTrue(resolve("Alpha.md", "本日", "Alpha.md 本日"))
        self.assertTrue(resolve("Alpha.md", "本日", "Alpha.md本日"))
        self.assertFalse(resolve("Alpha.md", "本日", "Alpha.md extra"))
        self.assertFalse(resolve("Alpha.md", "本日", "Alpha.md本日 extra"))
        self.assertFalse(resolve("Delta.md", "1/2(金)", "Delta.md2026/1/2(金)"))
        self.assertFalse(resolve("Alpha.md", "本日", "_Alpha.md 本日"))

    def test_long_name_split_uses_full_recognized_line_geometry(self):
        prefix_match = {
            "matched_text": "This Is",
            "recognized_line": "This Is An Extremely Long…",
            "bounding_box": {"minX": 0.10, "maxX": 0.20, "minY": 0.50, "maxY": 0.52},
            "line_bounding_box": {"minX": 0.10, "maxX": 0.60, "minY": 0.49, "maxY": 0.53},
        }
        geometry = mod.display_geometry_match(prefix_match, use_full_line=True)
        self.assertEqual(geometry["bounding_box"], prefix_match["line_bounding_box"])
        badge_over_later_text = {
            "bounding_box": {"minX": 0.55, "maxX": 0.65, "minY": 0.50, "maxY": 0.52}
        }
        self.assertFalse(mod.badge_is_strictly_right(geometry, badge_over_later_text))
        self.assertTrue(
            mod.badge_is_strictly_right(
                geometry,
                {"bounding_box": {"minX": 0.60, "maxX": 0.68, "minY": 0.50, "maxY": 0.52}},
            )
        )
        with self.assertRaises(ValueError):
            mod.display_geometry_match({"bounding_box": prefix_match["bounding_box"]}, use_full_line=True)

    def test_long_name_joined_uses_independent_match_geometry_not_badge_position(self):
        # Vision joined the truncated filename and badge into one
        # recognized_line. Deriving filename geometry by clipping to the
        # badge under verification's own position made the right-side check
        # an unfalsifiable tautology (Issue #185 follow-up false-PASS): the
        # badge always satisfies `badge.minX >= clipped_maxX` because the
        # clip endpoint *is* that same badge's minX, regardless of its true
        # overlap. The regex match itself is independent of the badge and
        # must be used unmodified.
        prefix_match = {
            "matched_text": "This Is An Extremely Long…",
            "recognized_line": "This Is An Extremely Long…本日",
            "bounding_box": {"minX": 0.10, "maxX": 0.50, "minY": 0.50, "maxY": 0.52},
            "line_bounding_box": {"minX": 0.10, "maxX": 0.65, "minY": 0.49, "maxY": 0.53},
        }
        geometry = mod.display_geometry_match(prefix_match, use_full_line=True, joined_row=True)
        self.assertEqual(geometry["bounding_box"], prefix_match["bounding_box"])

        # Regression 1: filename match maxX=0.50, actual joined badge
        # minX=0.55 (to the right, no overlap) -> pass.
        actual_badge_right = {"bounding_box": {"minX": 0.55, "maxX": 0.65, "minY": 0.50, "maxY": 0.52}}
        self.assertTrue(mod.badge_is_strictly_right(geometry, actual_badge_right))

        # Regression 2: filename match maxX=0.58 overlaps the actual joined
        # badge minX=0.55 -> fail. This is the very badge under
        # verification, not a different candidate; before the fix, clipping
        # the filename geometry to this badge's own minX made this
        # unconditionally pass regardless of the overlap.
        overlapping_match = {
            **prefix_match,
            "bounding_box": {"minX": 0.10, "maxX": 0.58, "minY": 0.50, "maxY": 0.52},
        }
        overlapping_geometry = mod.display_geometry_match(overlapping_match, use_full_line=True, joined_row=True)
        same_joined_badge = {"bounding_box": {"minX": 0.55, "maxX": 0.65, "minY": 0.50, "maxY": 0.52}}
        self.assertFalse(mod.badge_is_strictly_right(overlapping_geometry, same_joined_badge))

    def test_long_name_text_pattern_covers_trailing_ellipsis_but_not_badge(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, cases = mod.make_fixtures(Path(tmp) / "work-folder", today=dt.date(2026, 9, 17))
        pattern = next(
            case["text_pattern"] for case in cases if case["name"] == "long_name_keeps_badge_visible"
        )
        match = re.search(pattern, "This Is An Extremely Long Sidebar Filename…本日")
        self.assertIsNotNone(match)
        self.assertTrue(match.group(0).endswith("…"))
        self.assertNotIn("本日", match.group(0))

    def test_badge_must_not_overlap_display_name(self):
        text = {"bounding_box": {"minX": 0.10, "maxX": 0.20, "minY": 0.50, "maxY": 0.52}}
        touching = {"bounding_box": {"minX": 0.20, "maxX": 0.25, "minY": 0.50, "maxY": 0.52}}
        separated = {"bounding_box": {"minX": 0.201, "maxX": 0.25, "minY": 0.50, "maxY": 0.52}}
        overlapping = {"bounding_box": {"minX": 0.199, "maxX": 0.25, "minY": 0.50, "maxY": 0.52}}
        self.assertTrue(mod.badge_is_strictly_right(text, touching))
        self.assertTrue(mod.badge_is_strictly_right(text, separated))
        self.assertFalse(mod.badge_is_strictly_right(text, overlapping))

    def test_fixture_names_cover_required_shapes(self):
        with tempfile.TemporaryDirectory() as tmp:
            before, cases = mod.make_fixtures(Path(tmp) / "work-folder")
            names = {case["name"] for case in cases}
            self.assertEqual(
                names,
                {
                    "date_at_start",
                    "date_in_middle",
                    "date_at_end",
                    "one_digit_month_day",
                    "long_name_keeps_badge_visible",
                },
            )
            self.assertTrue(all(case["date_token"] in case["filename"] for case in cases))
            regular_cases = [case for case in cases if case["expected_display"] is not None]
            self.assertTrue(all(case["text_pattern"].startswith("^\\s*") for case in regular_cases))
            # The pattern only anchors the display-name prefix and has no
            # trailing boundary, so it still matches when Vision joins the
            # filename and badge into one recognized_line with no separating
            # space (e.g. `Alpha.md本日`, Issue #185 follow-up). Rejecting
            # any extra suffix/badge mismatch is the job of the whole-line
            # exact/joined composition checks downstream, not this pattern.
            self.assertFalse(any(case["text_pattern"].endswith("$") for case in regular_cases))
            self.assertEqual(before, sorted(case["filename"] for case in cases))
            self.assertTrue(all((Path(tmp) / "work-folder" / name).is_file() for name in before))

    def test_make_fixtures_exercises_all_relative_label_shapes_for_fixed_today(self):
        # Fixed `today` makes this deterministic instead of depending on which
        # calendar day the real GUI run happens to execute on.
        today = dt.date(2026, 9, 17)
        with tempfile.TemporaryDirectory() as tmp:
            _, cases = mod.make_fixtures(Path(tmp) / "work-folder", today=today)
        by_name = {case["name"]: case for case in cases}

        self.assertEqual(by_name["date_at_start"]["date_token"], "2026-09-17")
        self.assertEqual(by_name["date_at_start"]["badge_label"], "本日")
        self.assertEqual(by_name["date_in_middle"]["date_token"], "2026-09-01")
        self.assertEqual(by_name["date_in_middle"]["badge_label"], "1日(火)")
        self.assertEqual(by_name["one_digit_month_day"]["date_token"], "2026-1-2")
        self.assertEqual(by_name["one_digit_month_day"]["badge_label"], "1/2(金)")
        self.assertEqual(by_name["date_at_end"]["date_token"], "2025-09-02")
        self.assertEqual(by_name["date_at_end"]["badge_label"], "2025/9/2(火)")
        self.assertEqual(by_name["long_name_keeps_badge_visible"]["date_token"], "2026-09-17")
        self.assertEqual(by_name["long_name_keeps_badge_visible"]["badge_label"], "本日")

        labels = [case["badge_label"] for case in cases]
        shapes = set()
        for label in labels:
            if label == "本日":
                shapes.add("today")
            elif re.fullmatch(r"\d+日\(.\)", label):
                shapes.add("same_year_same_month")
            elif re.fullmatch(r"\d+/\d+\(.\)", label):
                shapes.add("same_year_other_month")
            elif re.fullmatch(r"\d+/\d+/\d+\(.\)", label):
                shapes.add("other_year")
        self.assertEqual(
            shapes,
            {"today", "same_year_same_month", "same_year_other_month", "other_year"},
        )


if __name__ == "__main__":
    unittest.main()
