"""Helper provenance checks without touching a screen or invoking Swift."""
import hashlib
import inspect
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import hosted_gui_interaction as interaction


class HelperTests(unittest.TestCase):
    def test_target_source_replacement_is_not_executed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'helper.swift'
            source.write_text('trusted source')
            def compile_helper(argv, **kwargs):
                Path(argv[-1]).write_bytes(b'trusted executable')
            with patch.object(interaction.subprocess, 'run', side_effect=compile_helper):
                helper = interaction.prepare_helper(source, root)
            source.write_text('target build.rs replaced source')
            with patch.object(interaction.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, 'ok', '')) as execute:
                self.assertTrue(interaction.run_helper(helper, ['ocr'], 1)[0])
                self.assertEqual(execute.call_args.args[0], [str(helper.binary), 'ocr'])

    def test_binary_replacement_before_or_during_execution_is_blocked(self):
        for when in ('before', 'during'):
            with self.subTest(when=when), tempfile.TemporaryDirectory() as directory:
                binary = Path(directory) / 'helper'
                binary.write_bytes(b'trusted')
                helper = interaction.PreparedHelper(binary, hashlib.sha256(b'trusted').hexdigest())
                if when == 'before':
                    binary.write_bytes(b'replaced')
                def execute(*args, **kwargs):
                    binary.write_bytes(b'replaced')
                    return subprocess.CompletedProcess([], 0, 'forged pass', '')
                with patch.object(interaction.subprocess, 'run', side_effect=execute) as run:
                    ok, output, reason = interaction.run_helper(helper, ['ocr'], 1)
                    self.assertFalse(ok)
                    self.assertEqual(output, '')
                    self.assertIn('integrity mismatch', reason)
                    self.assertEqual(run.call_count, int(when == 'during'))

    def test_body_crop_uses_cgimage_top_left_coordinates(self):
        source = Path(interaction.__file__).with_name('hosted_gui_interaction.swift').read_text()
        self.assertIn('let topInset = CGFloat(cgImage.height) * 0.15', source)
        self.assertRegex(source, r'CGRect\(\s*x: 0,\s*y: topInset,')
        self.assertNotIn('VNImageRectForNormalizedRect(normalizedBody', source)


class InlineSyntaxExpectationTests(unittest.TestCase):
    """Pure source-byte and visible-anchor logic for inline_syntax_boundary."""

    def test_boundary_insertions_match_the_fixture(self):
        text = interaction.INLINE_FIXTURE_ORIGINAL
        cases = [
            (interaction.BOLD_ITALIC_OPEN_RE, 'end', text.replace('**bold', '**Zbold', 1)),
            (interaction.BOLD_ITALIC_CLOSE_RE, 'start', text.replace('combo**', 'comboZ**', 1)),
            (interaction.CODE_SPAN_OPEN_RE, 'end', text.replace('`code', '`Zcode', 1)),
            (interaction.CODE_SPAN_CLOSE_RE, 'start', text.replace('span`', 'spanZ`', 1)),
            (interaction.QUOTE_BOLD_OPEN_RE, 'end', text.replace('quote with **bold', 'quote with **Zbold', 1)),
            (interaction.LIST_ITALIC_OPEN_RE, 'end', text.replace('item with *italic', 'item with *Zitalic', 1)),
        ]
        for pattern, edge, expected in cases:
            with self.subTest(pattern=pattern, edge=edge):
                self.assertEqual(interaction.insert_at_match(text, pattern, 'Z', edge=edge), expected)

    def test_boundary_ime_expected_bytes_use_the_same_hidden_marker_boundary(self):
        text = interaction.INLINE_FIXTURE_ORIGINAL
        expected = text.replace('**bold', '**日本語bold', 1)
        self.assertEqual(
            interaction.insert_at_match(text, interaction.BOLD_ITALIC_OPEN_RE,
                                        interaction.IME_EXPECTED_TEXT, edge='end'),
            expected,
        )

    def test_unmatched_pattern_is_rejected_rather_than_silently_skipped(self):
        with self.assertRaises(ValueError):
            interaction.insert_at_match(interaction.INLINE_FIXTURE_ORIGINAL, r'not-present', 'Z', edge='end')

    def test_ocr_locators_match_visible_text_without_hidden_markers(self):
        rendered_lines = [
            'bold italic combo boundary line.',
            'this inline code',
            'span crosses a line.',
            'quote with bold inside.',
            'list item with italic inside.',
        ]
        patterns = [
            interaction.BOLD_ITALIC_OCR_RE,
            interaction.CODE_SPAN_OPEN_OCR_RE,
            interaction.CODE_SPAN_CLOSE_OCR_RE,
            interaction.QUOTE_BOLD_OPEN_OCR_RE,
            interaction.LIST_ITALIC_OPEN_OCR_RE,
            interaction.DRAG_SELECT_START_OCR_RE,
            interaction.DRAG_SELECT_END_OCR_RE,
        ]
        for pattern in patterns:
            with self.subTest(pattern=pattern):
                self.assertEqual(sum(bool(re.search(pattern, line)) for line in rendered_lines), 1)
                self.assertNotRegex(pattern, r'\\[\*`]')

    def test_drag_selection_expected_bytes_cross_hidden_inner_markers(self):
        text = interaction.INLINE_FIXTURE_ORIGINAL
        selected = 'old *italic* com'
        self.assertIn(selected, text)
        self.assertEqual(text.replace(selected, '', 1),
                         text.replace('**bold *italic* combo**', '**bbo**', 1))

    def test_bold_italic_and_quote_source_patterns_target_different_lines(self):
        text = interaction.INLINE_FIXTURE_ORIGINAL
        bold_italic_pos = re.search(interaction.BOLD_ITALIC_OPEN_RE, text).start()
        quote_pos = re.search(interaction.QUOTE_BOLD_OPEN_RE, text).start()
        self.assertNotEqual(bold_italic_pos, quote_pos)
        self.assertIn('*italic*', text[bold_italic_pos:text.index('\n', bold_italic_pos)])
        self.assertIn('quote with', text[:quote_pos].rsplit('\n', 1)[-1])

    def test_each_required_delimiter_has_closed_and_unclosed_exact_bytes(self):
        for delimiter in ('*', '**', '`'):
            with self.subTest(delimiter=delimiter):
                unclosed, closed = interaction.delimiter_states(delimiter)
                self.assertEqual(unclosed, interaction.INLINE_FIXTURE_ORIGINAL + f' {delimiter}loose tail')
                self.assertEqual(closed, interaction.INLINE_FIXTURE_ORIGINAL + f' {delimiter}loose{delimiter} tail')
                closing_at = closed.rindex(delimiter, len(interaction.INLINE_FIXTURE_ORIGINAL))
                self.assertEqual(closed[:closing_at] + closed[closing_at + len(delimiter):], unclosed)

    def test_unknown_delimiter_is_rejected(self):
        with self.assertRaises(ValueError):
            interaction.delimiter_states('~~~')

    def test_multiline_code_span_close_toggle_targets_the_shared_parse_span(self):
        text = interaction.INLINE_FIXTURE_ORIGINAL
        self.assertIn('`code\nspan`', text)
        closing_at = re.search(interaction.CODE_SPAN_CLOSE_RE, text).start()
        unclosed = text[:closing_at] + text[closing_at + 1:]
        self.assertEqual(unclosed, text.replace('span`', 'span', 1))
        source = inspect.getsource(interaction.run_multiline_code_span_toggle_step)
        self.assertIn("INLINE_FIXTURE_ORIGINAL.replace(\"span`\", \"span\", 1)", source)
        self.assertIn('CODE_SPAN_CLOSE_OCR_RE', source)
        self.assertIn('image_pixel_digest', source)

    def test_inline_syntax_scenario_exercises_the_existing_multiline_code_span(self):
        source = inspect.getsource(interaction.run_inline_syntax_scenario)
        self.assertIn('run_multiline_code_span_toggle_step(', source)


class MatchInsertionOffsetTests(unittest.TestCase):
    """Issue #137: the canonical source offset used to build the expected
    fixture bytes must be the same offset used later to judge where an
    actual click landed."""

    def test_offset_reproduces_insert_at_match(self):
        text = interaction.INLINE_FIXTURE_ORIGINAL
        cases = [
            (interaction.BOLD_ITALIC_OPEN_RE, 'end'),
            (interaction.BOLD_ITALIC_CLOSE_RE, 'start'),
            (interaction.CODE_SPAN_OPEN_RE, 'end'),
            (interaction.CODE_SPAN_CLOSE_RE, 'start'),
            (interaction.QUOTE_BOLD_OPEN_RE, 'end'),
            (interaction.LIST_ITALIC_OPEN_RE, 'end'),
        ]
        for pattern, edge in cases:
            with self.subTest(pattern=pattern, edge=edge):
                offset = interaction.match_insertion_offset(text, pattern, edge=edge)
                self.assertEqual(
                    text[:offset] + 'Z' + text[offset:],
                    interaction.insert_at_match(text, pattern, 'Z', edge=edge),
                )

    def test_unmatched_pattern_is_rejected(self):
        with self.assertRaises(ValueError):
            interaction.match_insertion_offset(interaction.INLINE_FIXTURE_ORIGINAL, r'not-present', edge='end')


class LocateSingleInsertionOffsetTests(unittest.TestCase):
    """Issue #137: separate a clean single-character landing at the wrong
    offset (a deterministic, explainable outcome that can be compared against
    the expected canonical offset) from bytes that cannot be explained as a
    single insertion at all."""

    def test_locates_the_unique_offset(self):
        self.assertEqual(
            interaction.locate_single_insertion_offset('combo** boundary', 'comboZ** boundary', 'Z'), 5)

    def test_locates_the_offset_observed_in_run_34744353175(self):
        # PR #132 の trusted GUI run で実際に観測された着地: **bold *italic* combo**
        # の閉じ境界クリックが期待の "comboZ**" ではなく "combo** Zboundary" になった。
        baseline = interaction.INLINE_FIXTURE_ORIGINAL
        actual = baseline.replace('combo** boundary', 'combo** Zboundary', 1)
        expected_offset = interaction.match_insertion_offset(
            baseline, interaction.BOLD_ITALIC_CLOSE_RE, edge='start')
        landing_offset = interaction.locate_single_insertion_offset(baseline, actual, 'Z')
        self.assertIsNotNone(landing_offset)
        self.assertEqual(landing_offset - expected_offset, 3)  # "**" と直後の空白1文字を飛び越えた

    def test_returns_none_when_length_rules_out_a_single_insertion(self):
        self.assertIsNone(
            interaction.locate_single_insertion_offset('combo** boundary', 'combo** boundary and more', 'Z'))

    def test_returns_none_when_the_offset_is_ambiguous(self):
        # 'a' の連続に 'a' を挿入すると、どの位置に挿入しても同じ結果になるため
        # 単一の着地点として特定できない。
        self.assertIsNone(interaction.locate_single_insertion_offset('aaaa', 'aaaaa', 'a'))


class ParseClickEvidenceTests(unittest.TestCase):
    """Issue #137: click-text の stdout から OCR bounding box と実クリック座標を
    evidence として取り出す。取り出せない場合は helper 自身の契約違反として
    procedure blocked に倒せるよう ValueError にする。"""

    VALID_STDOUT = json.dumps({
        'matched_text': 'combo',
        'bounding_box': {'minX': 0.1, 'maxX': 0.2, 'minY': 0.3, 'maxY': 0.4},
        'window_bounds': {'x': 0.0, 'y': 0.0, 'width': 800.0, 'height': 600.0},
        'click_point': {'x': 123.0, 'y': 456.0},
        'edge': 'end',
    })

    def test_parses_well_formed_evidence(self):
        evidence = interaction.parse_click_evidence(self.VALID_STDOUT)
        self.assertEqual(evidence['matched_text'], 'combo')
        self.assertEqual(evidence['click_point'], {'x': 123.0, 'y': 456.0})

    def test_rejects_non_json_stdout(self):
        with self.assertRaises(ValueError):
            interaction.parse_click_evidence('clicked at 1,2 for pattern combo edge=end')

    def test_rejects_json_missing_required_fields(self):
        with self.assertRaises(ValueError):
            interaction.parse_click_evidence(json.dumps({'matched_text': 'combo'}))


class BoundaryEditCheckLandingClassificationTests(unittest.TestCase):
    """Issue #137: click-text の evidence を残しつつ、境界クリック挿入の着地点が
    期待 canonical position からどれだけ離れているかで「helper 自身が意図した
    visual boundary へ到達できなかった疑い(procedure blocked)」と「製品 source
    mapping の疑い(fail、#101 で切り分け)」を区別する。"""

    OCR_EVIDENCE = json.dumps({
        'matched_text': 'combo',
        'bounding_box': {'minX': 0.1, 'maxX': 0.2, 'minY': 0.3, 'maxY': 0.4},
        'window_bounds': {'x': 0.0, 'y': 0.0, 'width': 800.0, 'height': 600.0},
        'click_point': {'x': 123.0, 'y': 456.0},
        'edge': 'start',
    })

    def _run(self, directory, final_fixture_content):
        fixture_path = Path(directory) / 'fixture.md'
        baseline = interaction.INLINE_FIXTURE_ORIGINAL
        fixture_path.write_text(baseline, encoding='utf-8')
        calls = []

        def fake_run_helper(swift_helper, args, timeout):
            calls.append(args[0])
            if args[0] == 'click-text':
                return True, self.OCR_EVIDENCE, ''
            if args[0] == 'type-save':
                fixture_path.write_text(final_fixture_content, encoding='utf-8')
                return True, '', ''
            if args[0] == 'undo-save':
                fixture_path.write_text(baseline, encoding='utf-8')
                return True, '', ''
            raise AssertionError(f'unexpected helper call: {args}')

        with patch.object(interaction, 'run_helper', side_effect=fake_run_helper):
            status, reason, detail = interaction.boundary_edit_check(
                None, Path('/unused.png'), 1234, fixture_path, baseline,
                interaction.BOLD_ITALIC_OCR_RE, 'end',
                interaction.BOLD_ITALIC_CLOSE_RE, 'start', 'Z',
                1.0, 0.0,
            )
        return status, reason, detail, calls

    def test_exact_canonical_landing_passes_and_records_click_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = interaction.INLINE_FIXTURE_ORIGINAL
            expected = interaction.insert_at_match(
                baseline, interaction.BOLD_ITALIC_CLOSE_RE, 'Z', edge='start')
            status, reason, detail, calls = self._run(directory, expected)
        self.assertEqual(status, 'pass')
        self.assertIsNone(reason)
        self.assertEqual(detail['landing_classification'], 'at_canonical')
        self.assertEqual(detail['click_evidence']['matched_text'], 'combo')
        self.assertEqual(detail['actual_landing_source_offset'], detail['expected_canonical_source_offset'])
        self.assertEqual(calls, ['click-text', 'type-save', 'undo-save'])

    def test_matched_bytes_with_inconsistent_recomputed_offset_is_blocked(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = interaction.INLINE_FIXTURE_ORIGINAL
            expected = interaction.insert_at_match(
                baseline, interaction.BOLD_ITALIC_CLOSE_RE, 'Z', edge='start')
            with patch.object(interaction, 'locate_single_insertion_offset', return_value=999999):
                status, reason, detail, calls = self._run(directory, expected)
        self.assertEqual(status, 'blocked')
        self.assertEqual(detail['landing_classification'], 'corrupted')
        self.assertEqual(detail['actual_landing_source_offset'], 999999)
        self.assertNotIn('undo-save', calls)

    def test_near_canonical_mismatch_is_blocked_without_independent_coordinate_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = interaction.INLINE_FIXTURE_ORIGINAL
            actual = baseline.replace('combo** boundary', 'combo** Zboundary', 1)
            status, reason, detail, calls = self._run(directory, actual)
        self.assertEqual(status, 'blocked')
        self.assertEqual(detail['landing_classification'], 'boundary_ambiguous_near_canonical')
        self.assertEqual(detail['landing_offset_delta'], 3)
        self.assertIn('#101', reason)
        self.assertNotIn('undo-save', calls)

    def test_far_miss_is_blocked_instead_of_attributed_to_the_product(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = interaction.INLINE_FIXTURE_ORIGINAL
            actual = baseline.replace('list item with', 'list Zitem with', 1)
            status, reason, detail, calls = self._run(directory, actual)
        self.assertEqual(status, 'blocked')
        self.assertEqual(detail['landing_classification'], 'far_miss')
        self.assertGreater(detail['landing_offset_delta'], interaction.BOUNDARY_LANDING_FAR_MISS_CHARS)
        self.assertNotIn('undo-save', calls)

    def test_corrupted_result_that_is_not_a_single_insertion_stays_a_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            status, reason, detail, calls = self._run(directory, 'totally unrelated content')
        self.assertEqual(status, 'fail')
        self.assertEqual(detail['landing_classification'], 'corrupted')

    def test_unmodified_fixture_after_insertion_is_blocked_as_not_inserted(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = interaction.INLINE_FIXTURE_ORIGINAL
            status, reason, detail, calls = self._run(directory, baseline)
        self.assertEqual(status, 'blocked')
        self.assertEqual(detail['landing_classification'], 'not_inserted')
        self.assertNotIn('undo-save', calls)

    def test_click_text_execution_failure_is_blocked_not_failed(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            baseline = interaction.INLINE_FIXTURE_ORIGINAL
            fixture_path.write_text(baseline, encoding='utf-8')
            calls = []

            def fake_run_helper(swift_helper, args, timeout):
                calls.append(args[0])
                if args[0] == 'click-text':
                    return False, '', 'OCR で対象文字列を認識できない'
                raise AssertionError(f'unexpected helper call: {args}')

            with patch.object(interaction, 'run_helper', side_effect=fake_run_helper):
                status, reason, detail = interaction.boundary_edit_check(
                    None, Path('/unused.png'), 1234, fixture_path, baseline,
                    interaction.BOLD_ITALIC_OCR_RE, 'end',
                    interaction.BOLD_ITALIC_CLOSE_RE, 'start', 'Z',
                    1.0, 0.0,
                )
        self.assertEqual(status, 'blocked')
        self.assertEqual(calls, ['click-text'])
        self.assertIn('OCR', reason)

    def test_type_save_execution_failure_is_blocked_not_failed(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            baseline = interaction.INLINE_FIXTURE_ORIGINAL
            fixture_path.write_text(baseline, encoding='utf-8')
            calls = []

            def fake_run_helper(swift_helper, args, timeout):
                calls.append(args[0])
                if args[0] == 'click-text':
                    return True, self.OCR_EVIDENCE, ''
                if args[0] == 'type-save':
                    return False, '', 'timeout waiting for helper integrity check'
                raise AssertionError(f'unexpected helper call: {args}')

            with patch.object(interaction, 'run_helper', side_effect=fake_run_helper):
                status, reason, detail = interaction.boundary_edit_check(
                    None, Path('/unused.png'), 1234, fixture_path, baseline,
                    interaction.BOLD_ITALIC_OCR_RE, 'end',
                    interaction.BOLD_ITALIC_CLOSE_RE, 'start', 'Z',
                    1.0, 0.0,
                )
        self.assertEqual(status, 'blocked')
        self.assertEqual(calls, ['click-text', 'type-save'])
        self.assertIn('timeout', reason)

    def test_malformed_click_evidence_is_blocked_before_typing_the_probe(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            baseline = interaction.INLINE_FIXTURE_ORIGINAL
            fixture_path.write_text(baseline, encoding='utf-8')
            calls = []

            def fake_run_helper(swift_helper, args, timeout):
                calls.append(args[0])
                if args[0] == 'click-text':
                    return True, 'clicked at 1,2 for pattern combo edge=end', ''
                raise AssertionError(f'unexpected helper call: {args}')

            with patch.object(interaction, 'run_helper', side_effect=fake_run_helper):
                status, reason, detail = interaction.boundary_edit_check(
                    None, Path('/unused.png'), 1234, fixture_path, baseline,
                    interaction.BOLD_ITALIC_OCR_RE, 'end',
                    interaction.BOLD_ITALIC_CLOSE_RE, 'start', 'Z',
                    1.0, 0.0,
                )
        self.assertEqual(status, 'blocked')
        self.assertEqual(calls, ['click-text'])
        self.assertNotIn('click_evidence', detail)


class RunBoundaryStepReproducibilityConfirmationTests(unittest.TestCase):
    """Issue #137 review (Codex, PR #139): a `blocked` near-canonical/far-miss
    landing gets a second OCR/click attempt recorded as extra evidence.
    run_boundary_step is the orchestration layer with access to a fresh
    screenshot, so the retry lives there rather than inside the single-attempt
    boundary_edit_check. A later review (Codex, PR #139) clarified that the
    retry reuses the same OCR->click helper path, so even a reproduced offset
    is not independent coordinate evidence and must stay `blocked`, never
    escalate to `fail`."""

    OCR_EVIDENCE = json.dumps({
        'matched_text': 'combo',
        'bounding_box': {'minX': 0.1, 'maxX': 0.2, 'minY': 0.3, 'maxY': 0.4},
        'window_bounds': {'x': 0.0, 'y': 0.0, 'width': 800.0, 'height': 600.0},
        'click_point': {'x': 123.0, 'y': 456.0},
        'edge': 'start',
    })

    def _run(self, directory, type_save_contents):
        """type_save_contents supplies the fixture bytes written by each
        successive type-save call, in order (attempt 1, then attempt 2 if a
        confirmation retry happens)."""
        fixture_path = Path(directory) / 'fixture.md'
        baseline = interaction.INLINE_FIXTURE_ORIGINAL
        fixture_path.write_text(baseline, encoding='utf-8')
        calls = []
        type_save_calls = {'count': 0}

        def fake_run_helper(swift_helper, args, timeout):
            calls.append(args[0])
            if args[0] == 'click-text':
                return True, self.OCR_EVIDENCE, ''
            if args[0] == 'type-save':
                content = type_save_contents[type_save_calls['count']]
                type_save_calls['count'] += 1
                fixture_path.write_text(content, encoding='utf-8')
                return True, '', ''
            if args[0] == 'undo-save':
                fixture_path.write_text(baseline, encoding='utf-8')
                return True, '', ''
            if args[0] == 'force-save':
                return True, '', ''
            if args[0] == 'move-doc-start':
                return True, '', ''
            raise AssertionError(f'unexpected helper call: {args}')

        def fake_capture_named(module, env, config, window_id, run_dir, label):
            return {'name': f'capture_{label}', 'result': 'pass', 'reason': None}

        config = SimpleNamespace(fixture_path=fixture_path)
        process_holder = {'process': SimpleNamespace(pid=1234)}
        checks = [(
            interaction.BOLD_ITALIC_OCR_RE, 'end',
            interaction.BOLD_ITALIC_CLOSE_RE, 'start',
        )]
        with patch.object(interaction, 'run_helper', side_effect=fake_run_helper), \
             patch.object(interaction, 'capture_named', side_effect=fake_capture_named):
            steps = interaction.run_boundary_step(
                None, {}, config, None, process_holder, 'window-1', Path(directory),
                'boundary_click_edit_bold_italic', checks, 1.0, 0.0,
            )
        return steps, calls, fixture_path, baseline

    def test_reproduced_mismatch_across_two_attempts_stays_blocked_not_fail(self):
        """PR #139 review (Codex): both attempts reuse the same OCR->click helper
        path, so a reproduced offset only proves the OCR/click error is systematic,
        not that the product's source mapping is wrong. Without an independent
        coordinate-specific verification, this must stay `blocked`, never `fail`."""
        with tempfile.TemporaryDirectory() as directory:
            mismatch = interaction.INLINE_FIXTURE_ORIGINAL.replace(
                'combo** boundary', 'combo** Zboundary', 1)
            steps, calls, _fixture_path, _baseline = self._run(directory, [mismatch, mismatch])
        final_step = next(s for s in steps if s['name'] == 'boundary_click_edit_bold_italic')
        self.assertEqual(final_step['result'], 'blocked')
        self.assertIn('reproducible_mismatch', final_step['reason'])
        self.assertNotEqual(final_step['result'], 'fail')
        confirm_check = next(s for s in steps if s['name'] == 'boundary_click_edit_bold_italic_0_confirm_check')
        self.assertEqual(confirm_check['result'], 'blocked')
        self.assertEqual(confirm_check['landing_classification'], 'boundary_ambiguous_near_canonical')
        restore_step = next(s for s in steps if s['name'] == 'boundary_click_edit_bold_italic_0_confirm_restore')
        self.assertEqual(restore_step['result'], 'pass')
        self.assertEqual(calls.count('click-text'), 2)
        self.assertEqual(calls.count('type-save'), 2)

    def test_retry_landing_at_canonical_stays_blocked_as_non_reproducible(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = interaction.INLINE_FIXTURE_ORIGINAL
            mismatch = baseline.replace('combo** boundary', 'combo** Zboundary', 1)
            canonical = interaction.insert_at_match(
                baseline, interaction.BOLD_ITALIC_CLOSE_RE, 'Z', edge='start')
            steps, _calls, _fixture_path, _baseline = self._run(directory, [mismatch, canonical])
        final_step = next(s for s in steps if s['name'] == 'boundary_click_edit_bold_italic')
        self.assertEqual(final_step['result'], 'blocked')
        self.assertIn('再現しなかった', final_step['reason'])

    def test_retry_landing_at_a_different_offset_stays_blocked_as_inconclusive(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = interaction.INLINE_FIXTURE_ORIGINAL
            mismatch = baseline.replace('combo** boundary', 'combo** Zboundary', 1)
            different_mismatch = baseline.replace('boundary line', 'boundaryZ line', 1)
            steps, _calls, _fixture_path, _baseline = self._run(
                directory, [mismatch, different_mismatch])
        final_step = next(s for s in steps if s['name'] == 'boundary_click_edit_bold_italic')
        self.assertEqual(final_step['result'], 'blocked')
        self.assertIn('別の非 canonical', final_step['reason'])

    def test_confirmation_is_skipped_when_baseline_restore_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            baseline = interaction.INLINE_FIXTURE_ORIGINAL
            fixture_path.write_text(baseline, encoding='utf-8')
            mismatch = baseline.replace('combo** boundary', 'combo** Zboundary', 1)

            def fake_run_helper(swift_helper, args, timeout):
                if args[0] == 'click-text':
                    return True, self.OCR_EVIDENCE, ''
                if args[0] == 'type-save':
                    fixture_path.write_text(mismatch, encoding='utf-8')
                    return True, '', ''
                if args[0] == 'force-save':
                    return True, '', ''
                if args[0] == 'undo-save':
                    return False, '', 'System Events を利用できない'
                raise AssertionError(f'unexpected helper call: {args}')

            def fake_capture_named(module, env, config, window_id, run_dir, label):
                return {'name': f'capture_{label}', 'result': 'pass', 'reason': None}

            config = SimpleNamespace(fixture_path=fixture_path)
            process_holder = {'process': SimpleNamespace(pid=1234)}
            checks = [(
                interaction.BOLD_ITALIC_OCR_RE, 'end',
                interaction.BOLD_ITALIC_CLOSE_RE, 'start',
            )]
            with patch.object(interaction, 'run_helper', side_effect=fake_run_helper), \
                 patch.object(interaction, 'capture_named', side_effect=fake_capture_named):
                steps = interaction.run_boundary_step(
                    None, {}, config, None, process_holder, 'window-1', Path(directory),
                    'boundary_click_edit_bold_italic', checks, 1.0, 0.0,
                )
        final_step = next(s for s in steps if s['name'] == 'boundary_click_edit_bold_italic')
        self.assertEqual(final_step['result'], 'blocked')
        self.assertIn('baseline 復元に失敗した', final_step['reason'])
        self.assertNotIn('boundary_click_edit_bold_italic_0_confirm_check',
                          {s['name'] for s in steps})

    def test_non_ambiguous_blocked_classification_is_not_retried(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = interaction.INLINE_FIXTURE_ORIGINAL
            steps, calls, _fixture_path, _baseline = self._run(directory, [baseline])
        final_step = next(s for s in steps if s['name'] == 'boundary_click_edit_bold_italic')
        self.assertEqual(final_step['result'], 'blocked')
        check_step = next(s for s in steps if s['name'] == 'boundary_click_edit_bold_italic_check_0')
        self.assertEqual(check_step['landing_classification'], 'not_inserted')
        self.assertEqual(calls.count('type-save'), 1)


class RestoreScenarioBaselineTests(unittest.TestCase):
    """Issue #136: fail/blocked mutating subtests must not leak state forward."""

    def test_restores_after_a_bounded_number_of_undo_save_calls(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            fixture_path.write_text('corrupted content', encoding='utf-8')
            undo_calls = []

            def fake_run_helper(swift_helper, args, timeout):
                if args[0] == 'force-save':
                    return True, '', ''
                if args[0] == 'undo-save':
                    undo_calls.append(args)
                    if len(undo_calls) >= 2:
                        fixture_path.write_text(interaction.INLINE_FIXTURE_ORIGINAL, encoding='utf-8')
                    return True, '', ''
                if args[0] == 'move-doc-start':
                    return True, '', ''
                raise AssertionError(f'unexpected helper call: {args}')

            with patch.object(interaction, 'run_helper', side_effect=fake_run_helper):
                step = interaction.restore_scenario_baseline(
                    None, 1234, fixture_path, interaction.INLINE_FIXTURE_ORIGINAL,
                    1.0, 0.0, 'boundary_click_edit_bold_italic_state_restore',
                )
            self.assertEqual(step['result'], 'pass')
            self.assertEqual(len(undo_calls), 2)
            self.assertEqual(fixture_path.read_text(encoding='utf-8'), interaction.INLINE_FIXTURE_ORIGINAL)

    def test_gives_up_and_reports_blocked_when_undo_never_converges(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            fixture_path.write_text('stuck content that undo never fixes', encoding='utf-8')
            undo_calls = []

            def fake_run_helper(swift_helper, args, timeout):
                if args[0] == 'force-save':
                    return True, '', ''
                if args[0] == 'undo-save':
                    undo_calls.append(args)
                    return True, '', ''
                raise AssertionError(f'unexpected helper call: {args}')

            with patch.object(interaction, 'run_helper', side_effect=fake_run_helper):
                step = interaction.restore_scenario_baseline(
                    None, 1234, fixture_path, interaction.INLINE_FIXTURE_ORIGINAL,
                    1.0, 0.0, 'drag_select_delete_undo_redo_state_restore',
                )
            self.assertEqual(step['result'], 'blocked')
            self.assertEqual(len(undo_calls), interaction.BASELINE_RESTORE_MAX_UNDOS)
            self.assertNotEqual(fixture_path.read_text(encoding='utf-8'), interaction.INLINE_FIXTURE_ORIGINAL)

    def test_reports_blocked_immediately_when_undo_save_itself_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            fixture_path.write_text('corrupted content', encoding='utf-8')

            def fake_run_helper(swift_helper, args, timeout):
                if args[0] == 'force-save':
                    return True, '', ''
                if args[0] == 'undo-save':
                    return False, '', 'System Events を利用できない'
                raise AssertionError(f'unexpected helper call: {args}')

            with patch.object(interaction, 'run_helper', side_effect=fake_run_helper):
                step = interaction.restore_scenario_baseline(
                    None, 1234, fixture_path, interaction.INLINE_FIXTURE_ORIGINAL,
                    1.0, 0.0, 'boundary_ime_input_state_restore',
                )
            self.assertEqual(step['result'], 'blocked')
            self.assertIn('System Events', step['reason'])

    def test_reports_blocked_when_force_save_itself_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            fixture_path.write_text(interaction.INLINE_FIXTURE_ORIGINAL, encoding='utf-8')

            def fake_run_helper(swift_helper, args, timeout):
                if args[0] == 'force-save':
                    return False, '', 'System Events を利用できない'
                raise AssertionError(f'unexpected helper call: {args}')

            with patch.object(interaction, 'run_helper', side_effect=fake_run_helper):
                step = interaction.restore_scenario_baseline(
                    None, 1234, fixture_path, interaction.INLINE_FIXTURE_ORIGINAL,
                    1.0, 0.0, 'boundary_ime_input_state_restore',
                )
            self.assertEqual(step['result'], 'blocked')
            self.assertIn('force-save', step['reason'])

    def test_unsaved_edit_left_by_a_failed_mutating_subtest_is_flushed_and_undone(self):
        """Issue #136 follow-up (PR #138 review): a mutating AppleScript can fail
        between its edit keystroke and its own save keystroke, leaving the fixture
        on disk still byte-identical to baseline while the real app has an unsaved
        edit in memory. Restoration must not treat that as already-clean; force-save
        must flush the pending edit to disk before the match is judged, so a real
        mismatch is discovered and undone."""
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            fixture_path.write_text(interaction.INLINE_FIXTURE_ORIGINAL, encoding='utf-8')
            undo_calls = []

            def fake_run_helper(swift_helper, args, timeout):
                if args[0] == 'force-save':
                    # The app had an uncommitted keystroke edit that the failed
                    # subtest never got to save; flushing it now reveals the drift.
                    fixture_path.write_text('corrupted content left in memory', encoding='utf-8')
                    return True, '', ''
                if args[0] == 'undo-save':
                    undo_calls.append(args)
                    fixture_path.write_text(interaction.INLINE_FIXTURE_ORIGINAL, encoding='utf-8')
                    return True, '', ''
                if args[0] == 'move-doc-start':
                    return True, '', ''
                raise AssertionError(f'unexpected helper call: {args}')

            with patch.object(interaction, 'run_helper', side_effect=fake_run_helper):
                step = interaction.restore_scenario_baseline(
                    None, 1234, fixture_path, interaction.INLINE_FIXTURE_ORIGINAL,
                    1.0, 0.0, 'boundary_click_edit_bold_italic_state_restore',
                )
            self.assertEqual(step['result'], 'pass')
            self.assertEqual(len(undo_calls), 1)
            self.assertEqual(fixture_path.read_text(encoding='utf-8'), interaction.INLINE_FIXTURE_ORIGINAL)

    def test_force_save_race_is_not_masked_by_a_stale_baseline_read(self):
        """Codex review on PR #138: save_session writes asynchronously in a
        background executor, so the very first read after force-save's
        keystroke can still show the old, baseline-matching disk bytes while
        the real (unsaved) edit is about to land a moment later. Accepting
        that stale read as a clean match would let the delayed write silently
        contaminate the next subtest, exactly like Issue #136."""
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            fixture_path.write_text(interaction.INLINE_FIXTURE_ORIGINAL, encoding='utf-8')
            undo_calls = []

            def fake_run_helper(swift_helper, args, timeout):
                if args[0] == 'force-save':
                    def land_late():
                        time.sleep(0.05)
                        fixture_path.write_text('corrupted content left in memory', encoding='utf-8')
                    threading.Thread(target=land_late).start()
                    return True, '', ''
                if args[0] == 'undo-save':
                    undo_calls.append(args)
                    fixture_path.write_text(interaction.INLINE_FIXTURE_ORIGINAL, encoding='utf-8')
                    return True, '', ''
                if args[0] == 'move-doc-start':
                    return True, '', ''
                raise AssertionError(f'unexpected helper call: {args}')

            with patch.object(interaction, 'run_helper', side_effect=fake_run_helper):
                step = interaction.restore_scenario_baseline(
                    None, 1234, fixture_path, interaction.INLINE_FIXTURE_ORIGINAL,
                    1.0, 1.0, 'boundary_click_edit_bold_italic_state_restore',
                )
            self.assertEqual(step['result'], 'pass')
            self.assertEqual(len(undo_calls), 1)
            self.assertEqual(fixture_path.read_text(encoding='utf-8'), interaction.INLINE_FIXTURE_ORIGINAL)

    def test_force_save_race_is_not_masked_by_a_write_slower_than_a_fixed_settle_window(self):
        """Codex review on PR #138 (2nd pass): a previous fix only held a match
        for a fixed short settle window before accepting it. An async write
        landing later than that fixed window, but still well inside the
        caller's poll_timeout budget, must still be caught rather than
        accepted as a clean baseline."""
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            fixture_path.write_text(interaction.INLINE_FIXTURE_ORIGINAL, encoding='utf-8')
            undo_calls = []

            def fake_run_helper(swift_helper, args, timeout):
                if args[0] == 'force-save':
                    def land_late():
                        time.sleep(0.7)
                        fixture_path.write_text('corrupted content left in memory', encoding='utf-8')
                    threading.Thread(target=land_late).start()
                    return True, '', ''
                if args[0] == 'undo-save':
                    undo_calls.append(args)
                    fixture_path.write_text(interaction.INLINE_FIXTURE_ORIGINAL, encoding='utf-8')
                    return True, '', ''
                if args[0] == 'move-doc-start':
                    return True, '', ''
                raise AssertionError(f'unexpected helper call: {args}')

            with patch.object(interaction, 'run_helper', side_effect=fake_run_helper):
                step = interaction.restore_scenario_baseline(
                    None, 1234, fixture_path, interaction.INLINE_FIXTURE_ORIGINAL,
                    1.0, 1.0, 'boundary_click_edit_bold_italic_state_restore',
                )
            self.assertEqual(step['result'], 'pass')
            self.assertEqual(len(undo_calls), 1)
            self.assertEqual(fixture_path.read_text(encoding='utf-8'), interaction.INLINE_FIXTURE_ORIGINAL)


class RunMutatingSubtestTests(unittest.TestCase):
    """Issue #136: the per-subtest orchestration wrapper around baseline restore."""

    def test_pass_outcome_is_returned_untouched_without_restore_attempt(self):
        steps = []
        with patch.object(interaction, 'run_helper') as run_helper:
            ok = interaction.run_mutating_subtest(
                steps, [{'name': 'boundary_caret_navigation', 'result': 'pass', 'reason': None}],
                'boundary_caret_navigation', {'process': SimpleNamespace(pid=1)}, None,
                Path('/unused'), interaction.INLINE_FIXTURE_ORIGINAL, 1.0, 0.0,
            )
            run_helper.assert_not_called()
        self.assertTrue(ok)
        self.assertEqual(steps, [{'name': 'boundary_caret_navigation', 'result': 'pass', 'reason': None}])

    def test_fail_outcome_is_kept_and_a_successful_restore_allows_continuing(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            fixture_path.write_text('boundary click landed at the wrong position', encoding='utf-8')
            steps = []

            def fake_run_helper(swift_helper, args, timeout):
                if args[0] == 'undo-save':
                    fixture_path.write_text(interaction.INLINE_FIXTURE_ORIGINAL, encoding='utf-8')
                    return True, '', ''
                return True, '', ''

            with patch.object(interaction, 'run_helper', side_effect=fake_run_helper):
                ok = interaction.run_mutating_subtest(
                    steps, [{'name': 'boundary_click_edit_bold_italic', 'result': 'fail',
                            'reason': '境界クリック挿入後の内容が期待値と一致しない'}],
                    'boundary_click_edit_bold_italic', {'process': SimpleNamespace(pid=1)}, None,
                    fixture_path, interaction.INLINE_FIXTURE_ORIGINAL, 1.0, 0.0,
                )
            self.assertTrue(ok)
            self.assertEqual(steps[0]['result'], 'fail')
            self.assertEqual(steps[0]['reason'], '境界クリック挿入後の内容が期待値と一致しない')
            self.assertEqual(steps[1]['name'], 'boundary_click_edit_bold_italic_state_restore')
            self.assertEqual(steps[1]['result'], 'pass')
            self.assertEqual(fixture_path.read_text(encoding='utf-8'), interaction.INLINE_FIXTURE_ORIGINAL)

    def test_failed_restore_stops_the_scenario_without_hiding_the_original_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / 'fixture.md'
            fixture_path.write_text('stuck content that undo never fixes', encoding='utf-8')
            steps = []
            with patch.object(interaction, 'run_helper', return_value=(True, '', '')):
                ok = interaction.run_mutating_subtest(
                    steps, [{'name': 'drag_select_delete_undo_redo', 'result': 'blocked',
                            'reason': 'undo に失敗した'}],
                    'drag_select_delete_undo_redo', {'process': SimpleNamespace(pid=1)}, None,
                    fixture_path, interaction.INLINE_FIXTURE_ORIGINAL, 1.0, 0.0,
                )
            self.assertFalse(ok)
            self.assertEqual(steps[0]['result'], 'blocked')
            self.assertEqual(steps[0]['reason'], 'undo に失敗した')
            self.assertEqual(steps[1]['name'], 'drag_select_delete_undo_redo_state_restore')
            self.assertEqual(steps[1]['result'], 'blocked')

    def test_missing_pid_is_fail_closed_without_touching_the_helper(self):
        steps = []
        with patch.object(interaction, 'run_helper') as run_helper:
            ok = interaction.run_mutating_subtest(
                steps, [{'name': 'boundary_ime_input', 'result': 'blocked', 'reason': 'blocked before edit'}],
                'boundary_ime_input', {'process': None}, None,
                Path('/unused'), interaction.INLINE_FIXTURE_ORIGINAL, 1.0, 0.0,
            )
            run_helper.assert_not_called()
        self.assertFalse(ok)
        self.assertEqual(steps[1]['name'], 'boundary_ime_input_state_restore')
        self.assertEqual(steps[1]['result'], 'blocked')


def _fake_gui_validate_module():
    return SimpleNamespace(Config=SimpleNamespace)


class InlineSyntaxScenarioContaminationTests(unittest.TestCase):
    """Issue #136 acceptance: a failed independent subtest must not leak into the next one."""

    def _run_scenario(self, base_run_dir, run_helper_side_effect, boundary_step_side_effect):
        module = _fake_gui_validate_module()
        calls = []

        def fake_open_session(module, env, config, binary_path, process_holder, capture_label):
            process_holder['process'] = SimpleNamespace(pid=4242)
            return [{'name': 'launch', 'result': 'pass', 'reason': None}], 'window-1'

        def fake_close_session(module, env, process_holder):
            return {'name': 'cleanup', 'result': 'pass', 'reason': None}

        def fake_capture_named(module, env, config, window_id, run_dir, label):
            return {'name': f'capture_{label}', 'result': 'pass', 'reason': None}

        def make_pass_subtest(name):
            def fake(*args, **kwargs):
                calls.append(name)
                return [{'name': name, 'result': 'pass', 'reason': None}]
            return fake

        def fake_delimiter_toggle(module, env, config, swift_helper, process_holder, window_id, run_dir,
                                  kind, delimiter, helper_timeout, poll_timeout):
            name = f'delimiter_toggle_{kind}'
            calls.append(name)
            return [{'name': name, 'result': 'pass', 'reason': None}]

        with patch.object(interaction, 'open_session', side_effect=fake_open_session), \
             patch.object(interaction, 'close_session', side_effect=fake_close_session), \
             patch.object(interaction, 'capture_named', side_effect=fake_capture_named), \
             patch.object(interaction, 'run_boundary_step', side_effect=boundary_step_side_effect(calls)), \
             patch.object(interaction, 'run_boundary_navigation_step',
                          side_effect=make_pass_subtest('boundary_caret_navigation')), \
             patch.object(interaction, 'run_boundary_ime_step',
                          side_effect=make_pass_subtest('boundary_ime_input')), \
             patch.object(interaction, 'run_drag_select_step',
                          side_effect=make_pass_subtest('drag_select_delete_undo_redo')), \
             patch.object(interaction, 'run_delimiter_toggle_step', side_effect=fake_delimiter_toggle), \
             patch.object(interaction, 'run_multiline_code_span_toggle_step',
                          side_effect=make_pass_subtest('multiline_code_span_close_toggle')), \
             patch.object(interaction, 'run_helper', side_effect=run_helper_side_effect):
            result = interaction.run_inline_syntax_scenario(
                module, {}, Path('/unused-target'), None, base_run_dir, Path('/unused-binary'),
                'deadbeef', 'req-1', 1.0, 1.0, 1.0, 0.0, {'fail': 0, 'blocked': 1, 'pass': 2},
            )
        return result, calls

    def test_next_independent_subtest_starts_from_pristine_baseline_after_a_fail(self):
        fixture_path = None
        observed_fixture_at_second_subtest = {}

        def boundary_step_side_effect(calls):
            def fake(module, env, config, swift_helper, process_holder, window_id, run_dir,
                    name, checks, helper_timeout, poll_timeout):
                calls.append(name)
                if name == 'boundary_click_edit_bold_italic':
                    config.fixture_path.write_text(
                        interaction.INLINE_FIXTURE_ORIGINAL.replace('combo**', 'combo** Z', 1),
                        encoding='utf-8',
                    )
                    return [{'name': name, 'result': 'fail',
                            'reason': '境界クリック挿入後の内容が期待値と一致しない'}]
                observed_fixture_at_second_subtest.setdefault(name, config.fixture_path.read_text(encoding='utf-8'))
                return [{'name': name, 'result': 'pass', 'reason': None}]
            return fake

        def fake_run_helper(swift_helper, args, timeout):
            if args[0] == 'undo-save':
                fixture_path.write_text(interaction.INLINE_FIXTURE_ORIGINAL, encoding='utf-8')
                return True, '', ''
            return True, '', ''

        with tempfile.TemporaryDirectory() as directory:
            base_run_dir = Path(directory)
            fixture_path = base_run_dir / 'inline_syntax_boundary' / 'inline-fixture.md'
            result, calls = self._run_scenario(base_run_dir, fake_run_helper, boundary_step_side_effect)

        self.assertIn('boundary_click_edit_code_span', observed_fixture_at_second_subtest)
        self.assertEqual(observed_fixture_at_second_subtest['boundary_click_edit_code_span'],
                         interaction.INLINE_FIXTURE_ORIGINAL)
        restore_step = next(s for s in result['steps']
                            if s['name'] == 'boundary_click_edit_bold_italic_state_restore')
        self.assertEqual(restore_step['result'], 'pass')
        primary_fail_step = next(s for s in result['steps'] if s['name'] == 'boundary_click_edit_bold_italic')
        self.assertEqual(primary_fail_step['result'], 'fail')
        self.assertEqual(result['result'], 'fail')
        for name in ('boundary_click_edit_code_span', 'boundary_click_edit_quote', 'boundary_click_edit_list',
                    'boundary_caret_navigation', 'boundary_ime_input', 'drag_select_delete_undo_redo',
                    'delimiter_toggle_star', 'delimiter_toggle_bold', 'delimiter_toggle_code',
                    'multiline_code_span_close_toggle'):
            self.assertIn(name, calls, f'{name} should still run after the earlier subtest was restored')

    def test_restore_failure_blocks_all_remaining_subtests_and_keeps_the_original_failure(self):
        def boundary_step_side_effect(calls):
            def fake(module, env, config, swift_helper, process_holder, window_id, run_dir,
                    name, checks, helper_timeout, poll_timeout):
                calls.append(name)
                if name == 'boundary_click_edit_bold_italic':
                    config.fixture_path.write_text('stuck content that undo never fixes', encoding='utf-8')
                    return [{'name': name, 'result': 'fail',
                            'reason': '境界クリック挿入後の内容が期待値と一致しない'}]
                return [{'name': name, 'result': 'pass', 'reason': None}]
            return fake

        def fake_run_helper(swift_helper, args, timeout):
            return True, '', ''

        with tempfile.TemporaryDirectory() as directory:
            base_run_dir = Path(directory)
            result, calls = self._run_scenario(base_run_dir, fake_run_helper, boundary_step_side_effect)

        self.assertEqual(calls, ['boundary_click_edit_bold_italic'])
        restore_step = next(s for s in result['steps']
                            if s['name'] == 'boundary_click_edit_bold_italic_state_restore')
        self.assertEqual(restore_step['result'], 'blocked')
        primary_fail_step = next(s for s in result['steps'] if s['name'] == 'boundary_click_edit_bold_italic')
        self.assertEqual(primary_fail_step['result'], 'fail')
        self.assertEqual(result['result'], 'fail')
        skipped_names = {s['name']: s for s in result['steps'] if s['result'] == 'skipped'}
        for name in ('boundary_click_edit_code_span', 'boundary_click_edit_quote', 'boundary_click_edit_list',
                    'boundary_caret_navigation', 'boundary_ime_input', 'drag_select_delete_undo_redo',
                    'delimiter_toggle_star', 'delimiter_toggle_bold', 'delimiter_toggle_code',
                    'multiline_code_span_close_toggle'):
            self.assertIn(name, skipped_names, f'{name} should be skipped after a failed baseline restore')
            self.assertIn('復元', skipped_names[name]['reason'])


if __name__ == '__main__':
    unittest.main()
