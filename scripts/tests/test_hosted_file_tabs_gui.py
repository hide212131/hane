#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "hosted_file_tabs_gui.py"
SPEC = importlib.util.spec_from_file_location("hosted_file_tabs_gui", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
mod = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(mod)


class ProcedureIdentityTests(unittest.TestCase):
    def test_procedure_version_is_focused_version_one(self):
        self.assertEqual(mod.PROCEDURE_VERSION, "hosted-file-tabs/1")
        self.assertEqual(mod.VERIFICATION_KIND, "file_tabs_focused")

    def test_theme_pattern_matches_all_three_states(self):
        for label in ("System", "Light", "Dark"):
            self.assertRegex(f"Theme {label}", mod.THEME_PATTERN)

    def test_visible_tab_label_uses_fixture_stem(self):
        self.assertEqual(mod.FIXTURE_FILENAME, "file-tabs-focus.md")
        self.assertEqual(mod.FIXTURE_TAB_LABEL, "file-tabs-focus")


class ColorHelperTests(unittest.TestCase):
    def test_parse_hex_color_accepts_well_formed_value(self):
        self.assertEqual(mod.parse_hex_color("#1a2b3c"), (0x1A, 0x2B, 0x3C))

    def test_parse_hex_color_rejects_malformed_value(self):
        for bad in ("1a2b3c", "#1a2b3", "#gggggg", None, 123):
            with self.assertRaises(ValueError):
                mod.parse_hex_color(bad)

    def test_is_near_white_detects_true_white(self):
        self.assertTrue(mod.is_near_white("#ffffff"))

    def test_is_near_white_accepts_value_just_above_threshold(self):
        self.assertTrue(mod.is_near_white("#ebebeb"))

    def test_is_near_white_rejects_dark_color(self):
        self.assertFalse(mod.is_near_white("#202020"))

    def test_is_near_white_rejects_color_with_one_low_channel(self):
        # A near-white background with a single channel pulled dark (e.g. a
        # faint tint) must not be misclassified as near-white.
        self.assertFalse(mod.is_near_white("#fff000"))

    def test_colors_differ_true_for_light_and_dark_theme_backgrounds(self):
        self.assertTrue(mod.colors_differ("#f5f5f5", "#1e1e1e"))

    def test_colors_differ_false_for_same_color(self):
        self.assertFalse(mod.colors_differ("#303030", "#303030"))

    def test_colors_differ_false_for_imperceptible_noise(self):
        self.assertFalse(mod.colors_differ("#303030", "#313131"))


class ThemeLabelTests(unittest.TestCase):
    def test_extracts_lowercase_label(self):
        self.assertEqual(mod.theme_label_from_text("Theme System"), "system")
        self.assertEqual(mod.theme_label_from_text("Theme Light"), "light")
        self.assertEqual(mod.theme_label_from_text("Theme Dark"), "dark")

    def test_extracts_label_embedded_in_longer_ocr_line(self):
        self.assertEqual(
            mod.theme_label_from_text("Autosave on Theme Dark Recent:"), "dark"
        )

    def test_returns_none_when_absent(self):
        self.assertIsNone(mod.theme_label_from_text("Autosave on"))


class PathFragmentCoverageTests(unittest.TestCase):
    def test_full_coverage_for_single_unwrapped_fragment(self):
        path = "/Users/runner/work/file-tabs-focus.md"
        coverage = mod.path_fragment_coverage([path], path)
        self.assertEqual(coverage, 1.0)

    def test_full_coverage_for_wrapped_fragments_in_any_order(self):
        path = "/Users/runner/work/hane-file-tabs-gui/file-tabs-focus.md"
        first_half = path[: len(path) // 2]
        second_half = path[len(path) // 2 :]
        coverage = mod.path_fragment_coverage([second_half, first_half], path)
        self.assertEqual(coverage, 1.0)

    def test_partial_coverage_below_threshold(self):
        path = "/Users/runner/work/hane-file-tabs-gui/file-tabs-focus.md"
        coverage = mod.path_fragment_coverage([path[:5]], path)
        self.assertLess(coverage, mod.PATH_OCR_COVERAGE_MIN)

    def test_coverage_ignores_whitespace_from_line_wrap(self):
        path = "/tmp/file-tabs-focus.md"
        wrapped_as_two_lines = ["/tmp/file-tabs", "-focus.md"]
        coverage = mod.path_fragment_coverage(wrapped_as_two_lines, path)
        self.assertEqual(coverage, 1.0)

    def test_empty_expected_path_yields_zero_coverage(self):
        self.assertEqual(mod.path_fragment_coverage(["anything"], ""), 0.0)


class IconProbePlausibilityTests(unittest.TestCase):
    def _probe(self, **overrides) -> dict:
        probe = {
            "icon_pixel_rect": {"x0": 10, "y0": 10, "x1": 20, "y1": 20},
            "icon_center_fraction": {"x": 0.5, "y_from_top": 0.5},
            "contrast_run": 8,
            "contrast_pixel_count": 32,
        }
        probe.update(overrides)
        return probe

    def test_accepts_well_formed_probe(self):
        ok, reason = mod.icon_probe_is_plausible(self._probe())
        self.assertTrue(ok)
        self.assertIsNone(reason)

    def test_rejects_degenerate_single_pixel_icon(self):
        ok, reason = mod.icon_probe_is_plausible(
            self._probe(icon_pixel_rect={"x0": 10, "y0": 10, "x1": 11, "y1": 11})
        )
        self.assertFalse(ok)
        self.assertIsNotNone(reason)

    def test_rejects_implausibly_large_icon(self):
        ok, _ = mod.icon_probe_is_plausible(
            self._probe(icon_pixel_rect={"x0": 0, "y0": 0, "x1": 500, "y1": 500})
        )
        self.assertFalse(ok)

    def test_rejects_center_outside_unit_range(self):
        ok, _ = mod.icon_probe_is_plausible(
            self._probe(icon_center_fraction={"x": 1.2, "y_from_top": 0.5})
        )
        self.assertFalse(ok)

    def test_rejects_zero_contrast_run(self):
        ok, _ = mod.icon_probe_is_plausible(self._probe(contrast_run=0))
        self.assertFalse(ok)

    def test_rejects_sparse_contrast_noise(self):
        ok, _ = mod.icon_probe_is_plausible(
            self._probe(contrast_pixel_count=mod.ICON_MIN_CONTRAST_PIXELS - 1)
        )
        self.assertFalse(ok)

    def test_rejects_malformed_rect(self):
        ok, _ = mod.icon_probe_is_plausible(self._probe(icon_pixel_rect="not-a-rect"))
        self.assertFalse(ok)


class StepHelperTests(unittest.TestCase):
    def test_overall_result_prefers_fail_over_blocked_and_pass(self):
        priority = {"fail": 0, "blocked": 1, "pass": 2}
        steps = [
            {"result": "pass"},
            {"result": "blocked"},
            {"result": "fail"},
        ]
        self.assertEqual(mod.overall_result(steps, priority), "fail")

    def test_overall_result_blocked_when_no_steps(self):
        self.assertEqual(mod.overall_result([], {"fail": 0, "blocked": 1, "pass": 2}), "blocked")

    def test_reason_for_joins_non_pass_reasons(self):
        steps = [
            mod.step("a", "pass"),
            mod.step("b", "fail", "b failed"),
            mod.step("c", "blocked", "c blocked"),
            mod.skipped("d", "d skipped"),
        ]
        self.assertEqual(mod.reason_for(steps), "b failed; c blocked")

    def test_reason_for_default_when_all_pass(self):
        steps = [mod.step("a", "pass"), mod.skipped("b", "not needed")]
        self.assertIn("passed", mod.reason_for(steps))


if __name__ == "__main__":
    unittest.main()
