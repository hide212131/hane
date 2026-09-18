import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "hosted_normal_list_gui.py"
spec = importlib.util.spec_from_file_location("hosted_normal_list_gui", SCRIPT)
assert spec is not None and spec.loader is not None
mod = importlib.util.module_from_spec(spec)
sys.modules["hosted_normal_list_gui"] = mod
spec.loader.exec_module(mod)


class ProcedureIdentityTests(unittest.TestCase):
    def test_procedure_identity_is_focused_and_distinct(self):
        self.assertEqual(mod.PROCEDURE_VERSION, "hosted-normal-list/1")
        self.assertEqual(mod.VERIFICATION_KIND, "normal_list_focused")
        self.assertIn("#126", mod.SCOPE_NOTE)
        self.assertIn("hosted-gui-interaction/7", mod.SCOPE_NOTE)

    def test_all_scenarios_have_distinct_names_and_fixture_filenames(self):
        names = [spec_.name for spec_ in mod.ALL_SCENARIO_SPECS]
        filenames = [spec_.fixture_filename for spec_ in mod.ALL_SCENARIO_SPECS]
        self.assertEqual(len(names), len(set(names)))
        self.assertEqual(len(filenames), len(set(filenames)))
        self.assertEqual(len(mod.ALL_SCENARIO_SPECS), 3)


class PureByteTransformTests(unittest.TestCase):
    def test_match_insertion_offset_start_and_end(self):
        text = "abc def ghi"
        self.assertEqual(mod.match_insertion_offset(text, r"def", edge="start"), 4)
        self.assertEqual(mod.match_insertion_offset(text, r"def", edge="end"), 7)

    def test_match_insertion_offset_missing_pattern_raises(self):
        with self.assertRaises(ValueError):
            mod.match_insertion_offset("abc", r"zzz", edge="start")

    def test_insert_at_match_inserts_at_requested_edge(self):
        text = "Gamma row\n"
        self.assertEqual(mod.insert_at_match(text, r"Gamma row", " X", edge="end"), "Gamma row X\n")
        self.assertEqual(mod.insert_at_match(text, r"row", "X", edge="start"), "Gamma Xrow\n")

    def test_replace_unique_word_replaces_the_single_whole_word_match(self):
        text = "Child bullet one\nChild bullet two\n"
        self.assertEqual(mod.replace_unique_word(text, "one", "uno"), "Child bullet uno\nChild bullet two\n")

    def test_replace_unique_word_rejects_zero_occurrences(self):
        with self.assertRaises(ValueError):
            mod.replace_unique_word("no match here", "missing", "x")

    def test_replace_unique_word_rejects_ambiguous_multiple_occurrences(self):
        with self.assertRaises(ValueError):
            mod.replace_unique_word("one one", "one", "two")

    def test_replace_unique_word_uses_word_boundaries(self):
        # "one" inside "someone" must not be treated as a standalone match.
        self.assertEqual(mod.replace_unique_word("lone one", "one", "two"), "lone two")


class SequentialNumberingTests(unittest.TestCase):
    def test_expected_sequential_numbers_increments_from_seed(self):
        self.assertEqual(mod.expected_sequential_numbers(3, 3), [3, 4, 5])
        self.assertEqual(mod.expected_sequential_numbers(1, 1), [1])

    def test_expected_sequential_numbers_rejects_non_positive_count(self):
        with self.assertRaises(ValueError):
            mod.expected_sequential_numbers(1, 0)

    def test_nonsequential_ordered_fixture_expects_3_4_5(self):
        # Issue #126 D: non-1/non-sequential ordered source (3, 41, 100) must
        # display sequential normal numbers 3, 4, 5.
        self.assertEqual(mod.NONSEQUENTIAL_ORDERED_RAW_NUMBERS, (3, 41, 100))
        self.assertEqual(mod.NONSEQUENTIAL_ORDERED_EXPECTED_NUMBERS, (3, 4, 5))

    def test_leading_number_pattern_tolerates_ocr_separator_variants(self):
        import re
        pattern = mod.leading_number_pattern(4, r"Beta row")
        self.assertIsNotNone(re.search(pattern, "4. Beta row"))
        self.assertIsNotNone(re.search(pattern, "4 Beta row"))
        self.assertIsNotNone(re.search(pattern, "4)Beta row"))
        self.assertIsNone(re.search(pattern, "41. Beta row"))
        self.assertIsNone(re.search(pattern, "14. Beta row"))

    def test_line_has_number_token_matches_standalone_digits_only(self):
        self.assertTrue(mod.line_has_number_token("41. Beta row", 41))
        self.assertFalse(mod.line_has_number_token("41. Beta row", 4))
        self.assertFalse(mod.line_has_number_token("141. Beta row", 41))


class FixtureConstructionTests(unittest.TestCase):
    def test_mixed_nested_list_fixture_contains_expected_raw_markers(self):
        text = mod.build_mixed_nested_list_fixture()
        self.assertIn("- Alpha lead item", text)
        self.assertIn("  1. Alpha nested one", text)
        self.assertIn("  2. Alpha nested two", text)
        self.assertIn("- Beta lead item", text)
        self.assertIn("1. Gamma lead first", text)
        self.assertIn("   - Gamma nested bullet", text)
        self.assertIn("2. Gamma lead second", text)

    def test_paragraph_child_list_fixture_returns_to_parent_numbering(self):
        text = mod.build_paragraph_child_list_fixture()
        self.assertIn("1. First item intro line", text)
        self.assertIn("   - Child bullet one\n   - Child bullet two\n", text)
        self.assertIn("2. Second item after return", text)

    def test_nonsequential_ordered_fixture_uses_raw_nonsequential_numbers(self):
        text = mod.build_nonsequential_ordered_fixture()
        self.assertIn("3. Alpha row", text)
        self.assertIn("41. Beta row", text)
        self.assertIn("100. Gamma row", text)

    def test_every_scenario_check_pattern_is_found_in_its_own_baseline_text(self):
        import re
        for scenario in mod.ALL_SCENARIO_SPECS:
            with self.subTest(scenario=scenario.name):
                text = scenario.fixture_text
                for check in scenario.initial_checks:
                    self.assertIsNotNone(re.search(check.content_pattern, text))
                self.assertIsNotNone(re.search(scenario.marker_check.content_pattern, text))
                self.assertIsNotNone(re.search(scenario.marker_check.raw_marker_pattern, text))
                self.assertIsNotNone(re.search(scenario.edit_check.content_pattern, text))
                self.assertIsNotNone(re.search(scenario.ime_check.content_pattern, text))
                word_pattern = rf"\b{re.escape(scenario.selection_check.word)}\b"
                self.assertEqual(len(re.findall(word_pattern, text)), 1)


class ExpectedByteTransitionTests(unittest.TestCase):
    def test_mixed_nested_list_expected_state_chain(self):
        states = mod.compute_expected_states(mod.MIXED_NESTED_LIST_SPEC)
        self.assertIn("Gamma lead second EDITED-MIXED\n", states["after_edit"])
        self.assertIn("Alpha nested two日本語\n", states["after_ime"])
        self.assertIn("Gamma nested marker\n", states["after_select"])
        self.assertNotIn("Gamma nested bullet", states["after_select"])
        # Earlier mutations must survive later ones (cumulative, not replaced).
        self.assertIn("Gamma lead second EDITED-MIXED\n", states["after_select"])
        self.assertIn("Alpha nested two日本語\n", states["after_select"])

    def test_paragraph_child_list_expected_state_chain(self):
        states = mod.compute_expected_states(mod.PARAGRAPH_CHILD_LIST_SPEC)
        self.assertIn("2. Second item after return EDITED-CHILD\n", states["after_edit"])
        self.assertIn("First item continued paragraph.日本語\n", states["after_ime"])
        self.assertIn("Child bullet uno\n", states["after_select"])
        self.assertIn("2. Second item after return EDITED-CHILD\n", states["after_select"])

    def test_nonsequential_ordered_expected_state_chain_preserves_untouched_raw_markers(self):
        states = mod.compute_expected_states(mod.NONSEQUENTIAL_ORDERED_SPEC)
        self.assertIn("100. Gamma row EDITED-ORDERED\n", states["after_edit"])
        self.assertIn("3. Alpha row日本語\n", states["after_ime"])
        self.assertIn("41. Bravo row\n", states["after_select"])
        # The edit/ime mutations on Gamma/Alpha must not disturb the raw "41."
        # marker that selection-replacement later mutates only the word after.
        self.assertIn("41. Bravo row\n", states["after_select"])
        self.assertIn("100. Gamma row EDITED-ORDERED\n", states["after_select"])
        self.assertIn("3. Alpha row日本語\n", states["after_select"])

    def test_expected_state_chain_round_trips_through_undo(self):
        # undo after selection-replace must restore exactly `after_ime`.
        for scenario in mod.ALL_SCENARIO_SPECS:
            with self.subTest(scenario=scenario.name):
                states = mod.compute_expected_states(scenario)
                restored = mod.replace_unique_word(
                    states["after_select"], scenario.selection_check.replacement, scenario.selection_check.word
                )
                self.assertEqual(restored, states["after_ime"])


class InitialCheckEvaluationTests(unittest.TestCase):
    def test_pass_when_content_and_number_match(self):
        lines = ["some header", "4. Beta row", "trailer"]
        check = mod.InitialCheck("beta_row", r"Beta row", expected_number=4, forbidden_number=41)
        result = mod.evaluate_initial_check(lines, check)
        self.assertEqual(result["result"], "pass")
        self.assertEqual(result["name"], "initial_beta_row")

    def test_fail_when_content_missing(self):
        check = mod.InitialCheck("missing", r"Nowhere To Be Found")
        result = mod.evaluate_initial_check(["irrelevant line"], check)
        self.assertEqual(result["result"], "fail")
        self.assertIsNotNone(result["reason"])

    def test_fail_when_expected_number_does_not_match(self):
        lines = ["41. Beta row"]
        check = mod.InitialCheck("beta_row", r"Beta row", expected_number=4)
        result = mod.evaluate_initial_check(lines, check)
        self.assertEqual(result["result"], "fail")

    def test_fail_when_forbidden_raw_number_is_shown(self):
        lines = ["41. Beta row"]
        check = mod.InitialCheck("beta_row", r"Beta row", expected_number=41, forbidden_number=41)
        result = mod.evaluate_initial_check(lines, check)
        self.assertEqual(result["result"], "fail")

    def test_pass_without_number_constraints_only_checks_content(self):
        lines = ["Alpha lead item"]
        check = mod.InitialCheck("alpha_lead", r"Alpha lead item")
        result = mod.evaluate_initial_check(lines, check)
        self.assertEqual(result["result"], "pass")


class MarkerDisclosureEvaluationTests(unittest.TestCase):
    def test_marker_disclosure_pass_when_raw_marker_found_after_click(self):
        check = mod.MarkerCheck(content_pattern=r"Beta lead item", raw_marker_pattern=r"-\s*Beta lead item")
        result = mod.evaluate_marker_disclosure(["- Beta lead item"], check)
        self.assertEqual(result["result"], "pass")

    def test_marker_disclosure_fail_closed_when_raw_marker_absent(self):
        check = mod.MarkerCheck(content_pattern=r"Beta lead item", raw_marker_pattern=r"-\s*Beta lead item")
        result = mod.evaluate_marker_disclosure(["Beta lead item"], check)
        self.assertEqual(result["result"], "fail")

    def test_marker_reclosure_skipped_when_raw_and_normalized_are_indistinguishable(self):
        check = mod.MarkerCheck(content_pattern=r"Second item after return", raw_marker_pattern=r"2\.\s*Second item after return")
        result = mod.evaluate_marker_reclosure(["2. Second item after return"], check)
        self.assertEqual(result["result"], "skipped")

    def test_marker_reclosure_pass_when_normalized_number_reappears(self):
        check = mod.MarkerCheck(
            content_pattern=r"Beta row", raw_marker_pattern=r"41\.\s*Beta row", normalized_pattern=r"4\.\s*Beta row",
        )
        result = mod.evaluate_marker_reclosure(["4. Beta row"], check)
        self.assertEqual(result["result"], "pass")

    def test_marker_reclosure_fail_closed_when_raw_marker_still_shown(self):
        check = mod.MarkerCheck(
            content_pattern=r"Beta row", raw_marker_pattern=r"41\.\s*Beta row", normalized_pattern=r"4\.\s*Beta row",
        )
        result = mod.evaluate_marker_reclosure(["41. Beta row"], check)
        self.assertEqual(result["result"], "fail")

    def test_marker_reclosure_fail_closed_when_normalized_number_missing(self):
        check = mod.MarkerCheck(
            content_pattern=r"Beta row", raw_marker_pattern=r"41\.\s*Beta row", normalized_pattern=r"4\.\s*Beta row",
        )
        result = mod.evaluate_marker_reclosure(["Beta row"], check)
        self.assertEqual(result["result"], "fail")


class ResultAggregationTests(unittest.TestCase):
    def test_worst_prefers_fail_over_blocked_over_pass(self):
        priority = {"fail": 0, "blocked": 1, "pass": 2}
        self.assertEqual(mod.worst([{"result": "pass"}, {"result": "blocked"}], priority), "blocked")
        self.assertEqual(mod.worst([{"result": "pass"}, {"result": "fail"}, {"result": "blocked"}], priority), "fail")
        self.assertEqual(mod.worst([{"result": "pass"}], priority), "pass")

    def test_worst_defaults_to_blocked_when_no_considered_steps(self):
        priority = {"fail": 0, "blocked": 1, "pass": 2}
        self.assertEqual(mod.worst([{"result": "skipped"}], priority), "blocked")
        self.assertEqual(mod.worst([], priority), "blocked")

    def test_reason_for_joins_non_pass_reasons_only(self):
        steps = [
            {"result": "pass", "reason": None},
            {"result": "fail", "reason": "first problem"},
            {"result": "blocked", "reason": "second problem"},
        ]
        self.assertEqual(mod.reason_for(steps), "first problem; second problem")

    def test_reason_for_default_message_when_everything_passed(self):
        self.assertEqual(mod.reason_for([{"result": "pass", "reason": None}]), "すべての工程が成功した")


class OcrHelperFailClosedTests(unittest.TestCase):
    def test_ocr_lines_blocked_when_helper_run_fails(self):
        class FakeInteraction:
            @staticmethod
            def run_helper(helper, args, timeout):
                return False, "", "helper crashed"

        ok, lines, error = mod.ocr_lines(FakeInteraction(), object(), Path("shot.png"), 1.0)
        self.assertFalse(ok)
        self.assertEqual(lines, [])
        self.assertEqual(error, "helper crashed")

    def test_ocr_lines_splits_and_normalizes_whitespace(self):
        class FakeInteraction:
            @staticmethod
            def run_helper(helper, args, timeout):
                return True, "  4.  Beta   row  \n\nGamma row\n", ""

        ok, lines, _error = mod.ocr_lines(FakeInteraction(), object(), Path("shot.png"), 1.0)
        self.assertTrue(ok)
        self.assertEqual(lines, ["4. Beta row", "Gamma row"])

    def test_click_content_blocked_when_helper_run_fails(self):
        class FakeInteraction:
            @staticmethod
            def run_helper(helper, args, timeout):
                return False, "", "click helper failed"

        ok, evidence, error = mod.click_content(FakeInteraction(), object(), 123, Path("shot.png"), "pattern", "start", 1.0)
        self.assertFalse(ok)
        self.assertIsNone(evidence)
        self.assertEqual(error, "click helper failed")

    def test_click_content_blocked_on_malformed_evidence(self):
        class FakeInteraction:
            @staticmethod
            def run_helper(helper, args, timeout):
                return True, "not json", ""

        ok, evidence, error = mod.click_content(FakeInteraction(), object(), 123, Path("shot.png"), "pattern", "start", 1.0)
        self.assertFalse(ok)
        self.assertIsNone(evidence)
        self.assertIn("JSON", error)


class ClickEvidenceParsingTests(unittest.TestCase):
    def test_parse_click_evidence_accepts_complete_payload(self):
        import json
        payload = json.dumps({
            "matched_text": "Beta row", "bounding_box": {}, "window_bounds": {}, "click_point": {}, "edge": "start",
        })
        evidence = mod.parse_click_evidence(payload)
        self.assertEqual(evidence["matched_text"], "Beta row")

    def test_parse_click_evidence_rejects_invalid_json(self):
        with self.assertRaises(ValueError):
            mod.parse_click_evidence("{not json")

    def test_parse_click_evidence_rejects_missing_fields(self):
        import json
        with self.assertRaises(ValueError):
            mod.parse_click_evidence(json.dumps({"matched_text": "x"}))


if __name__ == "__main__":
    unittest.main()
