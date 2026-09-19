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
        self.assertEqual(mod.PROCEDURE_VERSION, "hosted-normal-list/4")
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
    def test_pass_when_child_bounding_box_is_right_of_baseline(self):
        baseline = {"bounding_box": {"minX": 0.1}}
        child = {"bounding_box": {"minX": 0.2}}
        result = mod.evaluate_nested_position("alpha", "Alpha nested one", baseline, child)
        self.assertEqual(result["result"], "pass")
        self.assertEqual(result["name"], "nested_position_alpha_Alpha nested one")

    def test_fail_closed_when_child_is_not_right_of_baseline(self):
        baseline = {"bounding_box": {"minX": 0.2}}
        child = {"bounding_box": {"minX": 0.2}}
        result = mod.evaluate_nested_position("alpha", "Alpha nested one", baseline, child)
        self.assertEqual(result["result"], "fail")

    def test_fail_closed_when_child_is_left_of_baseline(self):
        # 子項目が親から外れて平坦化された場合、子の x 位置は baseline と同じか左に来る。
        baseline = {"bounding_box": {"minX": 0.2}}
        child = {"bounding_box": {"minX": 0.05}}
        result = mod.evaluate_nested_position("alpha", "Alpha nested one", baseline, child)
        self.assertEqual(result["result"], "fail")

    def test_fail_closed_when_marker_width_alone_would_move_child_right_of_a_cross_kind_parent(self):
        # Issue #126 review, discussion_r4051650816: 実際には平坦(非ネスト)でも、ordered
        # marker("1.")は bullet marker("-")より幅が広いため、marker 種別が異なる親の本文 x を
        # そのまま基準にすると child_x が親の本文 x より右に出てしまい、誤って pass になり得る。
        # baseline は child と同じ ordered marker のトップレベル行なので、marker 幅の差は
        # 打ち消され、平坦なケースは正しく fail になる。
        bullet_parent_body_x = 0.12  # "-" (1 glyph) + space の直後
        ordered_child_body_x = 0.14  # 同じ物理位置に "1." (2 glyph) + space の直後で並んだだけ
        same_kind_ordered_baseline_x = ordered_child_body_x  # 同じ marker 種別・同じネスト深さ
        naive_result = mod.evaluate_nested_position(
            "alpha", "Alpha nested one",
            {"bounding_box": {"minX": bullet_parent_body_x}},
            {"bounding_box": {"minX": ordered_child_body_x}},
        )
        self.assertEqual(naive_result["result"], "pass", "regression fixture 自体が旧バグを再現できていない")
        fixed_result = mod.evaluate_nested_position(
            "alpha", "Alpha nested one",
            {"bounding_box": {"minX": same_kind_ordered_baseline_x}},
            {"bounding_box": {"minX": ordered_child_body_x}},
        )
        self.assertEqual(fixed_result["result"], "fail")


class MixedNestedListNestedPositionSpecTests(unittest.TestCase):
    def test_mixed_nested_list_spec_declares_alpha_and_gamma_nested_position_checks(self):
        labels = [check.label for check in mod.MIXED_NESTED_LIST_SPEC.nested_position_checks]
        self.assertEqual(labels, ["alpha", "gamma"])
        alpha = mod.MIXED_NESTED_LIST_SPEC.nested_position_checks[0]
        self.assertEqual(alpha.child_content_patterns, (r"Alpha nested one", r"Alpha nested two"))
        gamma = mod.MIXED_NESTED_LIST_SPEC.nested_position_checks[1]
        self.assertEqual(gamma.baseline_content_pattern, r"Beta lead item")
        self.assertEqual(gamma.child_content_patterns, (r"Gamma nested bullet", r"Gamma nested second"))

    def test_baseline_content_pattern_matches_the_same_marker_kind_as_its_children(self):
        # Alpha の子は ordered marker("1."/"2.")なので baseline も ordered marker の
        # トップレベル行("Gamma lead first")でなければならない。bullet の "Alpha lead item"
        # 自身を baseline にすると marker 幅の差で誤判定しうる(discussion_r4051650816)。
        alpha = mod.MIXED_NESTED_LIST_SPEC.nested_position_checks[0]
        self.assertEqual(alpha.baseline_content_pattern, r"Gamma lead first")
        # Gamma の子は bullet marker("-")なので baseline も bullet marker のトップレベル行
        # ("Beta lead item")でなければならない。
        gamma = mod.MIXED_NESTED_LIST_SPEC.nested_position_checks[1]
        self.assertEqual(gamma.baseline_content_pattern, r"Beta lead item")

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
    def test_blocked_when_baseline_click_fails_and_children_are_skipped(self):
        class FakeInteraction:
            @staticmethod
            def run_helper(helper, args, timeout):
                return False, "", "click helper failed"

        check = mod.NestedPositionCheck(
            label="alpha", baseline_content_pattern=r"Gamma lead first",
            child_content_patterns=(r"Alpha nested one",),
        )
        steps = mod.run_nested_position_checks(FakeInteraction(), object(), 123, Path("shot.png"), (check,), 1.0)
        names_results = {s["name"]: s["result"] for s in steps}
        self.assertEqual(names_results["nested_position_alpha_baseline_click"], "blocked")
        self.assertEqual(names_results["nested_position_alpha_Alpha nested one"], "skipped")

    def test_pass_when_baseline_and_child_clicks_succeed_with_child_to_the_right(self):
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
            label="alpha", baseline_content_pattern=r"Gamma lead first",
            child_content_patterns=(r"Alpha nested one",),
        )
        steps = mod.run_nested_position_checks(FakeInteraction(), object(), 123, Path("shot.png"), (check,), 1.0)
        result = next(s for s in steps if s["name"] == "nested_position_alpha_Alpha nested one")
        self.assertEqual(result["result"], "pass")

    def test_fail_closed_when_child_click_returns_malformed_evidence(self):
        import json

        class FakeInteraction:
            call_count = 0

            @staticmethod
            def run_helper(helper, args, timeout):
                FakeInteraction.call_count += 1
                if FakeInteraction.call_count == 1:
                    payload = json.dumps({
                        "matched_text": "x", "bounding_box": {"minX": 0.1}, "window_bounds": {},
                        "click_point": {}, "edge": "start",
                    })
                    return True, payload, ""
                return True, "not json", ""

        check = mod.NestedPositionCheck(
            label="alpha", baseline_content_pattern=r"Gamma lead first",
            child_content_patterns=(r"Alpha nested one",),
        )
        steps = mod.run_nested_position_checks(FakeInteraction(), object(), 123, Path("shot.png"), (check,), 1.0)
        result = next(s for s in steps if s["name"] == "nested_position_alpha_Alpha nested one")
        self.assertEqual(result["result"], "blocked")


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


class MatchInsertionOffsetTests(unittest.TestCase):
    def test_returns_start_offset_for_start_edge(self):
        self.assertEqual(mod.match_insertion_offset("41. Beta row", r"Beta row", edge="start"), 4)

    def test_returns_end_offset_for_end_edge(self):
        self.assertEqual(mod.match_insertion_offset("41. Beta row", r"Beta row", edge="end"), 12)

    def test_raises_when_pattern_not_found(self):
        with self.assertRaises(ValueError):
            mod.match_insertion_offset("41. Beta row", r"Nowhere", edge="start")


class InsertAtMatchTests(unittest.TestCase):
    def test_inserts_at_the_matched_start(self):
        self.assertEqual(mod.insert_at_match("41. Beta row", r"41", "9", edge="start"), "941. Beta row")


class CaretRightCountFromDocStartTests(unittest.TestCase):
    # PR #208 review PRRT_kwDOUETGuM6j8L1G: click-text の bounding box start/end は canonical
    # source caret 位置の証拠ではなく、本文クリック後の固定文字数移動は1文字の OCR 着地誤差
    # でも正常製品を fail にしうる。source marker/selection replace/IME commit の caret 位置は
    # すべて、document start(offset 0)からの純粋計算だけによる右移動回数で決定的に求め、
    # click-text/OCR には一切依存しない。
    def test_counts_characters_from_document_start_to_pattern(self):
        text = "41. Beta row"
        count = mod.caret_right_count_from_doc_start(text, r"Beta row", edge="start")
        self.assertEqual(count, 4)

    def test_raises_when_pattern_not_found(self):
        with self.assertRaises(ValueError):
            mod.caret_right_count_from_doc_start("41. Beta row", r"Nowhere", edge="start")

    def test_marker_direct_edit_offset_matches_raw_marker_start_in_editing_fixture(self):
        expected = mod.EDITING_FIXTURE_ORIGINAL.index("41. Beta row")
        self.assertEqual(mod.MARKER_DIRECT_EDIT_CARET_RIGHT_COUNT, expected)

    def test_ime_commit_offset_matches_alpha_row_end_in_editing_fixture(self):
        expected = mod.EDITING_FIXTURE_ORIGINAL.index("Alpha row") + len("Alpha row")
        self.assertEqual(mod.IME_COMMIT_CARET_RIGHT_COUNT, expected)

    def test_selection_replace_offset_matches_gamma_start_in_editing_fixture(self):
        expected = mod.EDITING_FIXTURE_ORIGINAL.index("Gamma")
        self.assertEqual(mod.SELECTION_REPLACE_CARET_RIGHT_COUNT, expected)


class SelectionReplaceWordLengthTests(unittest.TestCase):
    # Issue #126 post-merge GUI run 35410838091: OCR bounding box の start→end drag は
    # "Gamma" の末尾を取りこぼし "Deltaa" になった。選択範囲はキーボード(Shift+矢印)による
    # 既知の文字数で決定的に確定するため、その文字数自体が正しいことを固定する。
    def test_selection_replace_word_length_matches_the_literal_word(self):
        self.assertEqual(mod.SELECTION_REPLACE_WORD, "Gamma")
        self.assertEqual(mod.SELECTION_REPLACE_WORD_LENGTH, 5)

    def test_selection_replace_word_pattern_matches_the_literal_word(self):
        self.assertRegex(mod.SELECTION_REPLACE_WORD, mod.SELECTION_REPLACE_WORD_PATTERN)


class SelectionReplaceSubtestTests(unittest.TestCase):
    """PR #208 review PRRT_kwDOUETGuM6j8L1G: _selection_replace_subtest must position the
    selection start with keyboard-only move-doc-start/move-caret from document start, never
    click-text/OCR (a click-text call is a fixture regression, not a legitimate path)."""

    FIXTURE_PATH = Path("editing-fixture.md")

    def _make_interaction(self, *, move_doc_start_ok=True, move_caret_ok=True, capture_ok=True):
        calls = {"move_doc_start": 0, "move_caret": [], "shift_select": [], "capture": []}

        class FakeInteraction:
            @staticmethod
            def run_helper(helper, args, timeout):
                command = args[0]
                if command == "click-text":
                    raise AssertionError("click-text must not be used for selection-replace caret positioning")
                if command == "move-doc-start":
                    calls["move_doc_start"] += 1
                    return (True, "", "") if move_doc_start_ok else (False, "", "move-doc-start failed")
                if command == "move-caret":
                    calls["move_caret"].append(args)
                    return (True, "", "") if move_caret_ok else (False, "", "move-caret failed")
                if command == "shift-select":
                    calls["shift_select"].append(args)
                    return True, "", ""
                if command in ("type-save", "undo-save", "redo-save"):
                    return True, "", ""
                raise AssertionError(f"unexpected helper call: {args}")

            @staticmethod
            def wait_for_fixture_bytes(fixture_path, expected, timeout):
                return True, expected

            @staticmethod
            def capture_named(gui_validate_module, env, config, window_id, run_dir, label):
                calls["capture"].append(label)
                result = "pass" if capture_ok else "blocked"
                return {"name": f"capture_{label}", "result": result,
                        "reason": None if capture_ok else "capture failed"}

        return FakeInteraction(), calls

    def _run(self, interaction):
        return mod._selection_replace_subtest(
            object(), interaction, object(), object(), "window-1", object(), 4321,
            self.FIXTURE_PATH, Path("run"), 1.0, 1.0,
        )

    def test_positions_caret_via_keyboard_only_not_click_text(self):
        interaction, calls = self._make_interaction()
        steps = self._run(interaction)
        result = next(s for s in steps if s["name"] == "selection_replace_ascii_undo_redo")
        self.assertEqual(result["result"], "pass")
        self.assertEqual(calls["move_doc_start"], 1)
        self.assertEqual(len(calls["move_caret"]), 1)
        self.assertEqual(
            calls["move_caret"][0],
            ["move-caret", "4321", "right", str(mod.SELECTION_REPLACE_CARET_RIGHT_COUNT)],
        )
        self.assertEqual(
            calls["shift_select"],
            [["shift-select", "4321", "right", str(mod.SELECTION_REPLACE_WORD_LENGTH)]],
        )

    def test_blocked_when_move_doc_start_fails(self):
        interaction, calls = self._make_interaction(move_doc_start_ok=False)
        steps = self._run(interaction)
        result = next(s for s in steps if s["name"] == "selection_replace_ascii_undo_redo")
        self.assertEqual(result["result"], "blocked")
        self.assertEqual(len(calls["move_caret"]), 0)

    def test_blocked_when_move_caret_fails(self):
        interaction, calls = self._make_interaction(move_caret_ok=False)
        steps = self._run(interaction)
        result = next(s for s in steps if s["name"] == "selection_replace_ascii_undo_redo")
        self.assertEqual(result["result"], "blocked")
        self.assertEqual(calls["capture"], [])


class MarkerDirectEditSubtestTests(unittest.TestCase):
    """PR #208 review PRRT_kwDOUETGuM6j8L1G: _marker_direct_edit_subtest must position the
    raw-marker edit with keyboard-only move-doc-start/move-caret from document start, never
    click-text/OCR (a click-text call is a fixture regression, not a legitimate path)."""

    FIXTURE_PATH = Path("editing-fixture.md")

    def _make_interaction(self, *, move_doc_start_ok=True, move_caret_ok=True, capture_ok=True):
        calls = {"move_doc_start": 0, "move_caret": [], "capture": []}

        class FakeInteraction:
            @staticmethod
            def run_helper(helper, args, timeout):
                command = args[0]
                if command == "click-text":
                    raise AssertionError("click-text must not be used for marker caret positioning")
                if command == "move-doc-start":
                    calls["move_doc_start"] += 1
                    return (True, "", "") if move_doc_start_ok else (False, "", "move-doc-start failed")
                if command == "move-caret":
                    calls["move_caret"].append(args)
                    return (True, "", "") if move_caret_ok else (False, "", "move-caret failed")
                if command in ("type-save", "undo-save"):
                    return True, "", ""
                raise AssertionError(f"unexpected helper call: {args}")

            @staticmethod
            def wait_for_fixture_bytes(fixture_path, expected, timeout):
                return True, expected

            @staticmethod
            def capture_named(gui_validate_module, env, config, window_id, run_dir, label):
                calls["capture"].append(label)
                result = "pass" if capture_ok else "blocked"
                return {"name": f"capture_{label}", "result": result,
                        "reason": None if capture_ok else "capture failed"}

        return FakeInteraction(), calls

    def _run(self, interaction):
        return mod._marker_direct_edit_subtest(
            object(), interaction, object(), object(), "window-1", object(), 4321,
            self.FIXTURE_PATH, Path("run"), 1.0, 1.0,
        )

    def test_positions_caret_via_keyboard_only_not_click_text(self):
        interaction, calls = self._make_interaction()
        steps = self._run(interaction)
        result = next(s for s in steps if s["name"] == "direct_marker_edit_ascii_undo")
        self.assertEqual(result["result"], "pass")
        self.assertEqual(calls["move_doc_start"], 1)
        self.assertEqual(len(calls["move_caret"]), 1)
        self.assertEqual(
            calls["move_caret"][0],
            ["move-caret", "4321", "right", str(mod.MARKER_DIRECT_EDIT_CARET_RIGHT_COUNT)],
        )
        self.assertEqual(calls["capture"], ["marker_disclosed"])

    def test_blocked_when_move_doc_start_fails(self):
        interaction, calls = self._make_interaction(move_doc_start_ok=False)
        steps = self._run(interaction)
        result = next(s for s in steps if s["name"] == "direct_marker_edit_ascii_undo")
        self.assertEqual(result["result"], "blocked")
        self.assertEqual(len(calls["move_caret"]), 0)

    def test_blocked_when_move_caret_fails(self):
        interaction, calls = self._make_interaction(move_caret_ok=False)
        steps = self._run(interaction)
        result = next(s for s in steps if s["name"] == "direct_marker_edit_ascii_undo")
        self.assertEqual(result["result"], "blocked")
        self.assertEqual(calls["capture"], [])

    def test_blocked_when_disclosure_capture_fails(self):
        interaction, calls = self._make_interaction(capture_ok=False)
        steps = self._run(interaction)
        result = next(s for s in steps if s["name"] == "direct_marker_edit_ascii_undo")
        self.assertEqual(result["result"], "blocked")


class CaretMoveEditCheckTests(unittest.TestCase):
    """_caret_move_edit_check は click-text/OCR を一切使わない(Issue #126 post-merge GUI
    run 35410838091 の marker 直接編集の2回目の Vision OCR 誤認識に対する回帰固定)。"""

    BASELINE = "41. Beta row"
    SOURCE_PATTERN = r"41\.\s*Beta row"
    INSERTED = "941. Beta row"

    def _make_interaction(self, run_helper_results, wait_results):
        class FakeInteraction:
            @staticmethod
            def run_helper(helper, args, timeout):
                return run_helper_results[args[0]]

            @staticmethod
            def wait_for_fixture_bytes(fixture_path, expected, timeout):
                return wait_results[expected.decode("utf-8")]

        return FakeInteraction()

    def test_pass_when_insertion_and_undo_match(self):
        interaction = self._make_interaction(
            {"move-caret": (True, "", ""), "type-save": (True, "", ""), "undo-save": (True, "", "")},
            {self.INSERTED: (True, self.INSERTED.encode("utf-8")),
             self.BASELINE: (True, self.BASELINE.encode("utf-8"))},
        )
        status, reason, detail = mod._caret_move_edit_check(
            interaction, object(), 123, Path("fixture.md"), self.BASELINE,
            "left", 4, self.SOURCE_PATTERN, "start", "9", 1.0, 1.0,
        )
        self.assertEqual(status, "pass")
        self.assertIsNone(reason)
        self.assertEqual(detail["expected_canonical_source_offset"], 0)

    def test_blocked_when_move_caret_fails_without_typing(self):
        interaction = self._make_interaction({"move-caret": (False, "", "move failed")}, {})
        status, reason, _detail = mod._caret_move_edit_check(
            interaction, object(), 123, Path("fixture.md"), self.BASELINE,
            "left", 4, self.SOURCE_PATTERN, "start", "9", 1.0, 1.0,
        )
        self.assertEqual(status, "blocked")
        self.assertIn("move failed", reason)

    def test_blocked_when_type_save_fails(self):
        interaction = self._make_interaction(
            {"move-caret": (True, "", ""), "type-save": (False, "", "type failed")}, {},
        )
        status, reason, _detail = mod._caret_move_edit_check(
            interaction, object(), 123, Path("fixture.md"), self.BASELINE,
            "left", 4, self.SOURCE_PATTERN, "start", "9", 1.0, 1.0,
        )
        self.assertEqual(status, "blocked")
        self.assertIn("type failed", reason)

    def test_fail_closed_when_insertion_does_not_match_expected_bytes(self):
        interaction = self._make_interaction(
            {"move-caret": (True, "", ""), "type-save": (True, "", "")},
            {self.INSERTED: (False, b"unexpected")},
        )
        status, reason, _detail = mod._caret_move_edit_check(
            interaction, object(), 123, Path("fixture.md"), self.BASELINE,
            "left", 4, self.SOURCE_PATTERN, "start", "9", 1.0, 1.0,
        )
        self.assertEqual(status, "fail")
        self.assertIn("一致しない", reason)

    def test_fail_closed_when_undo_does_not_restore_baseline(self):
        interaction = self._make_interaction(
            {"move-caret": (True, "", ""), "type-save": (True, "", ""), "undo-save": (True, "", "")},
            {self.INSERTED: (True, self.INSERTED.encode("utf-8")),
             self.BASELINE: (False, self.INSERTED.encode("utf-8"))},
        )
        status, reason, _detail = mod._caret_move_edit_check(
            interaction, object(), 123, Path("fixture.md"), self.BASELINE,
            "left", 4, self.SOURCE_PATTERN, "start", "9", 1.0, 1.0,
        )
        self.assertEqual(status, "fail")
        self.assertIn("undo", reason)

    def test_skips_move_caret_call_when_count_is_zero(self):
        calls = []

        class FakeInteraction:
            @staticmethod
            def run_helper(helper, args, timeout):
                calls.append(args[0])
                return True, "", ""

            @staticmethod
            def wait_for_fixture_bytes(fixture_path, expected, timeout):
                return True, expected

        status, _reason, _detail = mod._caret_move_edit_check(
            FakeInteraction(), object(), 123, Path("fixture.md"), self.BASELINE,
            "left", 0, self.SOURCE_PATTERN, "start", "9", 1.0, 1.0,
        )
        self.assertEqual(status, "pass")
        self.assertNotIn("move-caret", calls)


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


class ReopenExpectationTests(unittest.TestCase):
    # Issue #126 review discussion_r4051650814: 再起動後の再オープン検証は編集済み
    # 通常リストの復元を確認する必要があり、元のフィクスチャ内容をそのまま確認しても
    # 変更済みリストの復元が壊れていることを検出できない。
    def test_reopen_expected_bytes_is_the_edited_state_not_the_original(self):
        self.assertEqual(mod.REOPEN_EXPECTED_BYTES, mod.EDITING_AFTER_SELECTION_REPLACE)
        self.assertNotEqual(mod.REOPEN_EXPECTED_BYTES, mod.EDITING_FIXTURE_ORIGINAL)

    def test_reopen_visible_text_identifies_edited_content_not_original(self):
        self.assertIn(mod.REOPEN_VISIBLE_TEXT, mod.EDITING_AFTER_SELECTION_REPLACE)
        self.assertNotIn(mod.REOPEN_VISIBLE_TEXT, mod.EDITING_FIXTURE_ORIGINAL)


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


class ImeCommitSubtestTests(unittest.TestCase):
    """Issue #126 focused GUI (PR #208 review PRRT_kwDOUETGuM6j8CB_, P1):
    currentSourceID() reporting the Japanese source is not proof that Hane's IME
    session can actually convert keystrokes yet. _ime_commit_subtest must retry
    with a small, bounded upper limit, restoring a pristine baseline (file + app
    state) and repositioning the caret before every attempt, rather than trusting
    a single conversion attempt or an unconditional retry."""

    FIXTURE_PATH = Path("editing-fixture.md")

    def _make_interaction(self, *, japanese_available=True, commit_effects=None,
                          restore_ok_until=None, move_caret_ok=True, current_source_ok=True):
        commit_effects = commit_effects or ["ok_matched"]
        calls = {
            "commit": 0, "restore": [], "capture": [], "move_caret": [],
            "deactivate": 0, "source_select": [],
        }
        current_source = {"id": "com.apple.keylayout.ABC"}

        class FakeInteraction:
            JAPANESE_SOURCE = "com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese"

            @staticmethod
            def run_helper(helper, args, timeout):
                command = args[0]
                if command == "current-source":
                    if not current_source_ok:
                        return False, "", "current-source failed"
                    return True, current_source["id"], ""
                if command == "list-sources":
                    sources = "com.apple.keylayout.ABC"
                    if japanese_available:
                        sources += f"\n{FakeInteraction.JAPANESE_SOURCE}"
                    return True, sources, ""
                if command == "deactivate":
                    calls["deactivate"] += 1
                    return True, "", ""
                if command == "select-source":
                    current_source["id"] = args[1]
                    calls["source_select"].append(args[1])
                    return True, "", ""
                if command == "click-text":
                    # PR #208 review PRRT_kwDOUETGuM6j8L1G: IME caret positioning must never
                    # use click-text/OCR. If production code calls it, the fixture regressed.
                    raise AssertionError("click-text must not be used for IME caret positioning")
                if command == "move-caret":
                    calls["move_caret"].append(args)
                    if not move_caret_ok:
                        return False, "", "move-caret failed"
                    return True, "", ""
                if command == "type-romaji-at-caret-commit-save":
                    index = calls["commit"]
                    calls["commit"] += 1
                    outcome = commit_effects[min(index, len(commit_effects) - 1)]
                    if outcome == "helper_fail":
                        return False, "", "commit helper failed"
                    return True, "", ""
                raise AssertionError(f"unexpected helper call: {args}")

            @staticmethod
            def select_input_source(helper, source_id, timeout):
                ok, _out, error = FakeInteraction.run_helper(
                    helper, ["select-source", source_id], timeout
                )
                if not ok:
                    return False, current_source["id"], error, 1
                return True, current_source["id"], "", 1

            @staticmethod
            def wait_for_fixture_bytes(fixture_path, expected, timeout):
                index = calls["commit"] - 1
                outcome = commit_effects[min(index, len(commit_effects) - 1)]
                if outcome == "ok_matched":
                    return True, expected
                return False, b"unconverted"

            @staticmethod
            def restore_scenario_baseline(helper, pid, fixture_path, baseline, helper_timeout, poll_timeout, name):
                calls["restore"].append(name)
                attempt = len(calls["restore"])
                result = "pass" if restore_ok_until is None or attempt <= restore_ok_until else "blocked"
                return {"name": name, "result": result,
                       "reason": None if result == "pass" else "baseline undo に失敗した"}

            @staticmethod
            def capture_named(gui_validate_module, env, config, window_id, run_dir, label):
                calls["capture"].append(label)
                return {"name": f"capture_{label}", "result": "pass", "reason": None}

        return FakeInteraction(), calls

    def _run(self, interaction):
        return mod._ime_commit_subtest(
            object(), interaction, object(), object(), "window-1", object(), 4321,
            self.FIXTURE_PATH, Path("run"), 1.0, 0.0,
        )

    def test_pass_on_first_attempt(self):
        interaction, calls = self._make_interaction(commit_effects=["ok_matched"])
        steps, original_source = self._run(interaction)
        self.assertEqual(original_source, "com.apple.keylayout.ABC")
        commit_step = next(s for s in steps if s["name"] == "direct_ime_commit_at_caret")
        self.assertEqual(commit_step["result"], "pass")
        self.assertEqual(len(commit_step["attempts"]), 1)
        self.assertEqual(calls["commit"], 1)
        self.assertEqual(len(calls["restore"]), 1)
        self.assertEqual(calls["deactivate"], 1)
        self.assertEqual(calls["source_select"], [interaction.JAPANESE_SOURCE])

    def test_second_attempt_succeeds_after_pristine_baseline_restore(self):
        interaction, calls = self._make_interaction(commit_effects=["ok_mismatch", "ok_matched"])
        steps, _original_source = self._run(interaction)
        commit_step = next(s for s in steps if s["name"] == "direct_ime_commit_at_caret")
        self.assertEqual(commit_step["result"], "pass")
        attempts = commit_step["attempts"]
        self.assertEqual(len(attempts), 2)
        self.assertFalse(attempts[0]["matched"])
        self.assertTrue(attempts[1]["matched"])
        # The failed first attempt's unconverted text must not survive into the
        # second attempt: a pristine baseline restore must have run in between.
        self.assertEqual(len(calls["restore"]), 2)

    def test_persistent_mismatch_across_all_attempts_is_a_fail_not_a_silent_pass(self):
        interaction, calls = self._make_interaction(
            commit_effects=["ok_mismatch"] * mod.IME_COMMIT_MAX_ATTEMPTS
        )
        steps, _original_source = self._run(interaction)
        commit_step = next(s for s in steps if s["name"] == "direct_ime_commit_at_caret")
        self.assertEqual(commit_step["result"], "fail")
        self.assertEqual(len(commit_step["attempts"]), mod.IME_COMMIT_MAX_ATTEMPTS)
        self.assertTrue(all(not a["matched"] for a in commit_step["attempts"]))
        self.assertEqual(calls["commit"], mod.IME_COMMIT_MAX_ATTEMPTS)

    def test_no_japanese_source_is_blocked_without_any_attempt(self):
        interaction, calls = self._make_interaction(japanese_available=False)
        steps, _original_source = self._run(interaction)
        identify_step = next(s for s in steps if s["name"] == "ime_commit_identify_japanese_source")
        self.assertEqual(identify_step["result"], "blocked")
        self.assertEqual(calls["commit"], 0)

    def test_baseline_restore_failure_blocks_without_a_further_attempt(self):
        interaction, calls = self._make_interaction(
            commit_effects=["ok_mismatch", "ok_matched"], restore_ok_until=1,
        )
        steps, _original_source = self._run(interaction)
        commit_step = next(s for s in steps if s["name"] == "direct_ime_commit_at_caret")
        self.assertEqual(commit_step["result"], "blocked")
        self.assertIn("baseline", commit_step["reason"])
        self.assertEqual(calls["commit"], 1)

    def test_commit_helper_failure_is_blocked_not_failed(self):
        interaction, calls = self._make_interaction(commit_effects=["helper_fail"])
        steps, _original_source = self._run(interaction)
        commit_step = next(s for s in steps if s["name"] == "direct_ime_commit_at_caret")
        self.assertEqual(commit_step["result"], "blocked")
        self.assertEqual(len(commit_step["attempts"]), 1)
        self.assertEqual(commit_step["attempts"][0]["helper_result"], "blocked")

    def test_each_attempt_captures_its_own_screenshot(self):
        interaction, calls = self._make_interaction(commit_effects=["ok_mismatch", "ok_matched"])
        self._run(interaction)
        self.assertEqual(calls["capture"], ["ime_commit_attempt_1", "ime_commit_attempt_2"])

    def test_attempt_budget_is_small_and_fixed(self):
        self.assertGreaterEqual(mod.IME_COMMIT_MAX_ATTEMPTS, 2)
        self.assertLessEqual(mod.IME_COMMIT_MAX_ATTEMPTS, 5)

    def test_positions_caret_via_move_caret_not_click_text(self):
        # PR #208 review PRRT_kwDOUETGuM6j8L1G: each attempt must reach caret position with a
        # keyboard-only move-caret from document start, never click-text/OCR (FakeInteraction
        # raises AssertionError above if click-text is ever called).
        interaction, calls = self._make_interaction(commit_effects=["ok_mismatch", "ok_matched"])
        self._run(interaction)
        self.assertEqual(len(calls["move_caret"]), 2)
        for args in calls["move_caret"]:
            self.assertEqual(args, ["move-caret", "4321", "right", str(mod.IME_COMMIT_CARET_RIGHT_COUNT)])

    def test_blocked_when_move_caret_fails(self):
        interaction, calls = self._make_interaction(move_caret_ok=False)
        steps, _original_source = self._run(interaction)
        commit_step = next(s for s in steps if s["name"] == "direct_ime_commit_at_caret")
        self.assertEqual(commit_step["result"], "blocked")
        self.assertIn("move-caret failed", commit_step["reason"])
        self.assertEqual(calls["commit"], 0)


if __name__ == "__main__":
    unittest.main()
