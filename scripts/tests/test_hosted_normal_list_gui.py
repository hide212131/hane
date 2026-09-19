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
        self.assertEqual(mod.PROCEDURE_VERSION, "hosted-normal-list/3")
        self.assertEqual(mod.VERIFICATION_KIND, "normal_list_focused")
        self.assertIn("#126", mod.SCOPE_NOTE)
        self.assertIn("hosted-gui-interaction/7", mod.SCOPE_NOTE)

    def test_all_scenarios_have_distinct_names_and_fixture_filenames(self):
        names = [spec_.name for spec_ in mod.ALL_SCENARIO_SPECS]
        filenames = [spec_.fixture_filename for spec_ in mod.ALL_SCENARIO_SPECS]
        self.assertEqual(len(names), len(set(names)))
        self.assertEqual(len(filenames), len(set(filenames)))
        self.assertEqual(len(mod.ALL_SCENARIO_SPECS), 3)


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


class DisplayOrderEvaluationTests(unittest.TestCase):
    def test_pass_when_patterns_appear_in_expected_top_to_bottom_order(self):
        lines = ["Alpha lead item", "1. Alpha nested one", "2. Alpha nested two", "Beta lead item"]
        patterns = [r"Alpha lead item", r"Alpha nested one", r"Alpha nested two", r"Beta lead item"]
        result = mod.evaluate_display_order(lines, patterns)
        self.assertEqual(result["result"], "pass")
        self.assertEqual(result["matched_line_indices"], [0, 1, 2, 3])

    def test_fail_closed_when_two_items_are_swapped(self):
        # "Beta lead item" と "Alpha nested two" が入れ替わった、mixed fixture が
        # 崩れた場合の再現(Issue #126 review discussion_r4051349760)。
        lines = ["Alpha lead item", "1. Alpha nested one", "Beta lead item", "2. Alpha nested two"]
        patterns = [r"Alpha lead item", r"Alpha nested one", r"Alpha nested two", r"Beta lead item"]
        result = mod.evaluate_display_order(lines, patterns)
        self.assertEqual(result["result"], "fail")

    def test_fail_closed_when_a_pattern_is_entirely_missing(self):
        lines = ["Alpha lead item", "1. Alpha nested one"]
        patterns = [r"Alpha lead item", r"Alpha nested one", r"Alpha nested two"]
        result = mod.evaluate_display_order(lines, patterns)
        self.assertEqual(result["result"], "fail")

    def test_find_ordered_line_indices_does_not_reuse_an_earlier_line(self):
        # 同じ行を 2 つの pattern が両方とも指す(=実質的に一方が別の場所に無い)場合は
        # 順序どおりに前進できず None になる。
        lines = ["Alpha lead item Beta lead item"]
        patterns = [r"Alpha lead item", r"Beta lead item"]
        self.assertIsNone(mod.find_ordered_line_indices(lines, patterns))


class NestedPositionEvaluationTests(unittest.TestCase):
    def test_pass_when_child_bounding_box_is_right_of_parent(self):
        parent = {"bounding_box": {"minX": 0.1}}
        child = {"bounding_box": {"minX": 0.2}}
        result = mod.evaluate_nested_position("alpha", "Alpha nested one", parent, child)
        self.assertEqual(result["result"], "pass")
        self.assertEqual(result["name"], "nested_position_alpha_Alpha nested one")

    def test_fail_closed_when_child_is_not_right_of_parent(self):
        parent = {"bounding_box": {"minX": 0.2}}
        child = {"bounding_box": {"minX": 0.2}}
        result = mod.evaluate_nested_position("alpha", "Alpha nested one", parent, child)
        self.assertEqual(result["result"], "fail")

    def test_fail_closed_when_child_is_left_of_parent(self):
        # 子項目が親から外れて平坦化された場合、子の x 位置は親と同じか左に来る。
        parent = {"bounding_box": {"minX": 0.2}}
        child = {"bounding_box": {"minX": 0.05}}
        result = mod.evaluate_nested_position("alpha", "Alpha nested one", parent, child)
        self.assertEqual(result["result"], "fail")


class MixedNestedListNestedPositionSpecTests(unittest.TestCase):
    def test_mixed_nested_list_spec_declares_alpha_and_gamma_nested_position_checks(self):
        labels = [check.label for check in mod.MIXED_NESTED_LIST_SPEC.nested_position_checks]
        self.assertEqual(labels, ["alpha", "gamma"])
        alpha = mod.MIXED_NESTED_LIST_SPEC.nested_position_checks[0]
        self.assertEqual(alpha.child_content_patterns, (r"Alpha nested one", r"Alpha nested two"))
        gamma = mod.MIXED_NESTED_LIST_SPEC.nested_position_checks[1]
        self.assertEqual(gamma.parent_content_pattern, r"Gamma lead first")
        self.assertEqual(gamma.child_content_patterns, (r"Gamma nested bullet", r"Gamma nested second"))

    def test_paragraph_and_nonsequential_specs_have_no_nested_position_checks(self):
        self.assertEqual(mod.PARAGRAPH_CHILD_LIST_SPEC.nested_position_checks, ())
        self.assertEqual(mod.NONSEQUENTIAL_ORDERED_SPEC.nested_position_checks, ())


class ParagraphChildListReturnsToParentNumberingTests(unittest.TestCase):
    def test_second_after_return_check_requires_normal_display_number_2(self):
        check = next(
            c for c in mod.PARAGRAPH_CHILD_LIST_SPEC.initial_checks if c.label == "second_after_return"
        )
        self.assertEqual(check.expected_number, 2)

    def test_second_after_return_check_passes_only_with_number_2(self):
        check = next(
            c for c in mod.PARAGRAPH_CHILD_LIST_SPEC.initial_checks if c.label == "second_after_return"
        )
        passing = mod.evaluate_initial_check(["2. Second item after return"], check)
        self.assertEqual(passing["result"], "pass")
        failing = mod.evaluate_initial_check(["3. Second item after return"], check)
        self.assertEqual(failing["result"], "fail")


class NestedPositionCheckRunnerTests(unittest.TestCase):
    def test_blocked_when_parent_click_fails_and_children_are_skipped(self):
        class FakeInteraction:
            @staticmethod
            def run_helper(helper, args, timeout):
                return False, "", "click helper failed"

        check = mod.NestedPositionCheck(
            label="alpha", parent_content_pattern=r"Alpha lead item",
            child_content_patterns=(r"Alpha nested one",),
        )
        steps = mod.run_nested_position_checks(FakeInteraction(), object(), 123, Path("shot.png"), (check,), 1.0)
        names_results = {s["name"]: s["result"] for s in steps}
        self.assertEqual(names_results["nested_position_alpha_parent_click"], "blocked")
        self.assertEqual(names_results["nested_position_alpha_Alpha nested one"], "skipped")

    def test_pass_when_parent_and_child_clicks_succeed_with_child_to_the_right(self):
        import json

        class FakeInteraction:
            call_count = 0

            @staticmethod
            def run_helper(helper, args, timeout):
                FakeInteraction.call_count += 1
                min_x = 0.1 if FakeInteraction.call_count == 1 else 0.2
                payload = json.dumps({
                    "matched_text": "x", "bounding_box": {"minX": min_x}, "window_bounds": {},
                    "click_point": {}, "edge": "start",
                })
                return True, payload, ""

        check = mod.NestedPositionCheck(
            label="alpha", parent_content_pattern=r"Alpha lead item",
            child_content_patterns=(r"Alpha nested one",),
        )
        steps = mod.run_nested_position_checks(FakeInteraction(), object(), 123, Path("shot.png"), (check,), 1.0)
        result = next(s for s in steps if s["name"] == "nested_position_alpha_Alpha nested one")
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


class SingleLineReplacementTests(unittest.TestCase):
    def test_replaces_the_unique_occurrence(self):
        self.assertEqual(
            mod.single_line_replacement("a\nb\nc\n", "b", "B"), "a\nB\nc\n",
        )

    def test_rejects_missing_occurrence(self):
        with self.assertRaises(ValueError):
            mod.single_line_replacement("a\nb\nc\n", "z", "Z")

    def test_rejects_ambiguous_occurrence(self):
        with self.assertRaises(ValueError):
            mod.single_line_replacement("a\na\n", "a", "A")


class EditingFixtureTests(unittest.TestCase):
    def test_editing_fixture_contains_source_marker_and_body_targets(self):
        self.assertIn("3. Alpha row", mod.EDITING_FIXTURE_ORIGINAL)
        self.assertIn("41. Beta row", mod.EDITING_FIXTURE_ORIGINAL)
        self.assertIn("100. Gamma row", mod.EDITING_FIXTURE_ORIGINAL)

    def test_body_direct_edit_only_changes_its_own_line(self):
        self.assertEqual(
            mod.EDITING_AFTER_BODY_DIRECT_EDIT,
            mod.EDITING_FIXTURE_ORIGINAL.replace("41. Beta row", "41. Beta row Z", 1),
        )

    def test_marker_direct_edit_only_changes_its_own_marker(self):
        self.assertEqual(
            mod.EDITING_AFTER_MARKER_DIRECT_EDIT,
            mod.EDITING_FIXTURE_ORIGINAL.replace("41. Beta row", "941. Beta row", 1),
        )

    def test_ime_commit_appends_expected_japanese_text_to_its_own_line(self):
        self.assertEqual(
            mod.EDITING_AFTER_IME_COMMIT,
            mod.EDITING_FIXTURE_ORIGINAL.replace("3. Alpha row", "3. Alpha row" + mod.IME_COMMIT_EXPECTED_TEXT, 1),
        )

    def test_selection_replace_only_changes_its_own_word(self):
        self.assertEqual(
            mod.EDITING_AFTER_SELECTION_REPLACE,
            mod.EDITING_FIXTURE_ORIGINAL.replace("100. Gamma row", "100. Delta row", 1),
        )

    def test_every_editing_expectation_differs_from_baseline_in_exactly_one_line(self):
        baseline_lines = mod.EDITING_FIXTURE_ORIGINAL.splitlines()
        for expected in (
            mod.EDITING_AFTER_BODY_DIRECT_EDIT,
            mod.EDITING_AFTER_MARKER_DIRECT_EDIT,
            mod.EDITING_AFTER_IME_COMMIT,
            mod.EDITING_AFTER_SELECTION_REPLACE,
        ):
            with self.subTest(expected=expected):
                expected_lines = expected.splitlines()
                self.assertEqual(len(expected_lines), len(baseline_lines))
                differing = [
                    index for index, (before, after) in enumerate(zip(baseline_lines, expected_lines))
                    if before != after
                ]
                self.assertEqual(len(differing), 1)


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
