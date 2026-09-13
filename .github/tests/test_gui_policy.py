"""GUI receipt boundary regressions; no network, agents, builds, or GUI input."""
from copy import deepcopy
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1] / 'scripts'
sys.path.insert(0, str(SCRIPTS))
import gui_policy as policy
from pipeline_api import GitHub

SHA, CONTROL = 'a' * 40, 'b' * 40
REQUEST = {'pr_number': 79, 'sha': SHA, 'control_sha': CONTROL, 'repository': 'owner/hane',
           'procedure_version': policy.PROCEDURE,
           'run_id': '123', 'run_attempt': '1', 'generation': '123-1', 'request_id': 'gui-123-1-pr79',
           'created_at': '2026-09-09T00:00:00+00:00', 'expires_at': '2026-09-09T01:00:00+00:00'}
NOW = datetime(2026, 9, 9, 0, 30, tzinfo=timezone.utc)
DEFAULT_IMAGE = b'\x89PNG\r\n\x1a\n' + b'x' * 200
BODY_CLOSED_IMAGE = b'\x89PNG\r\n\x1a\n' + b'c' * 200
BODY_UNCLOSED_IMAGE = b'\x89PNG\r\n\x1a\n' + b'u' * 200
BODY_CLOSED_SHA = hashlib.sha256(BODY_CLOSED_IMAGE).hexdigest()
BODY_UNCLOSED_SHA = hashlib.sha256(BODY_UNCLOSED_IMAGE).hexdigest()


def _inline_evidence(steps):
    by_name = {step['name']: step for step in steps}
    boundary = {
        'boundary_click_edit_bold_italic_check_0': (
            'inline_syntax_boundary/boundary_click_edit_bold_italic_0.png',
            policy.INLINE_FIXTURE_ORIGINAL.replace('**bold', '**Zbold', 1),
        ),
        'boundary_click_edit_bold_italic_check_1': (
            'inline_syntax_boundary/boundary_click_edit_bold_italic_1.png',
            policy.INLINE_FIXTURE_ORIGINAL.replace('combo**', 'comboZ**', 1),
        ),
        'boundary_click_edit_code_span_check_0': (
            'inline_syntax_boundary/boundary_click_edit_code_span_0.png',
            policy.INLINE_FIXTURE_ORIGINAL.replace('`code', '`Zcode', 1),
        ),
        'boundary_click_edit_code_span_check_1': (
            'inline_syntax_boundary/boundary_click_edit_code_span_1.png',
            policy.INLINE_FIXTURE_ORIGINAL.replace('span`', 'spanZ`', 1),
        ),
        'boundary_click_edit_quote_check_0': (
            'inline_syntax_boundary/boundary_click_edit_quote_0.png',
            policy.INLINE_FIXTURE_ORIGINAL.replace('quote with **bold', 'quote with **Zbold', 1),
        ),
        'boundary_click_edit_list_check_0': (
            'inline_syntax_boundary/boundary_click_edit_list_0.png',
            policy.INLINE_FIXTURE_ORIGINAL.replace('item with *italic', 'item with *Zitalic', 1),
        ),
        'boundary_ime_input_check': (
            'inline_syntax_boundary/boundary_ime_input.png',
            policy.INLINE_FIXTURE_ORIGINAL.replace('**bold', '**日本語bold', 1),
        ),
    }
    for name, (screenshot, inserted) in boundary.items():
        by_name[name].update(
            screenshot=screenshot,
            expected_after_insert=inserted, actual_after_insert=inserted,
            expected_after_undo=policy.INLINE_FIXTURE_ORIGINAL,
            actual_after_undo=policy.INLINE_FIXTURE_ORIGINAL,
        )

    navigation = {
        'boundary_caret_navigation_check_0': (
            'inline_syntax_boundary/boundary_caret_navigation_0.png',
            'left_across_open_marker', 'left',
            policy.INLINE_FIXTURE_ORIGINAL.replace('**bold', '*N*bold', 1),
        ),
        'boundary_caret_navigation_check_1': (
            'inline_syntax_boundary/boundary_caret_navigation_1.png',
            'right_into_visible_text', 'right',
            policy.INLINE_FIXTURE_ORIGINAL.replace('**bold', '**bNold', 1),
        ),
        'boundary_caret_navigation_check_2': (
            'inline_syntax_boundary/boundary_caret_navigation_2.png',
            'up_from_multiline_code_close', 'up',
            policy.INLINE_FIXTURE_ORIGINAL.replace('this inline `code', 'thisN inline `code', 1),
        ),
    }
    for name, (screenshot, case, direction, inserted) in navigation.items():
        by_name[name].update(
            screenshot=screenshot, case=case, direction=direction, count=1,
            expected_after_move_insert=inserted, actual_after_move_insert=inserted,
            expected_after_undo=policy.INLINE_FIXTURE_ORIGINAL,
            actual_after_undo=policy.INLINE_FIXTURE_ORIGINAL,
        )

    deleted = policy.INLINE_FIXTURE_ORIGINAL.replace('old *italic* com', '', 1)
    by_name['drag_select_delete_undo_redo_check'].update(
        screenshot='inline_syntax_boundary/drag_select_state0.png',
        deleted_expected=deleted, deleted_actual=deleted,
        undo_actual=policy.INLINE_FIXTURE_ORIGINAL, redo_actual=deleted,
        restored_actual=policy.INLINE_FIXTURE_ORIGINAL,
    )
    for kind, delimiter in (('star', '*'), ('bold', '**'), ('code', '`')):
        unclosed, closed = policy._delimiter_states(delimiter)
        by_name[f'delimiter_toggle_{kind}_check'].update(
            initial_screenshot=f'inline_syntax_boundary/delimiter_toggle_{kind}_initial_closed.png',
            unclosed_screenshot=f'inline_syntax_boundary/delimiter_toggle_{kind}_unclosed.png',
            closed_screenshot=f'inline_syntax_boundary/delimiter_toggle_{kind}_closed.png',
            delimiter=delimiter,
            initial_pixel_digest=BODY_CLOSED_SHA,
            unclosed_pixel_digest=BODY_UNCLOSED_SHA,
            closed_pixel_digest=BODY_CLOSED_SHA,
            visual_transition_observed=True,
            closed_visual_restored=True,
            unclosed_expected=unclosed, unclosed_actual=unclosed,
            closed_expected=closed, closed_actual=closed,
        )

    span_unclosed = policy.INLINE_FIXTURE_ORIGINAL.replace('span`', 'span', 1)
    by_name['multiline_code_span_close_toggle_check'].update(
        initial_screenshot='inline_syntax_boundary/multiline_code_span_close_toggle_initial_closed.png',
        unclosed_screenshot='inline_syntax_boundary/multiline_code_span_close_toggle_unclosed.png',
        closed_screenshot='inline_syntax_boundary/multiline_code_span_close_toggle_closed.png',
        initial_pixel_digest=BODY_CLOSED_SHA,
        unclosed_pixel_digest=BODY_UNCLOSED_SHA,
        closed_pixel_digest=BODY_CLOSED_SHA,
        visual_transition_observed=True,
        closed_visual_restored=True,
        unclosed_expected=span_unclosed, unclosed_actual=span_unclosed,
        closed_expected=policy.INLINE_FIXTURE_ORIGINAL, closed_actual=policy.INLINE_FIXTURE_ORIGINAL,
    )


def passing_result():
    scenarios = []
    for name, expected in policy.REQUIRED_STEPS.items():
        steps = [{'name': step, 'result': 'pass'} for step in sorted(expected)]
        if name == 'ascii_edit_save_undo_redo_reopen':
            steps.extend({'name': step, 'result': 'pass'} for step in ('launch', 'window_discovery', 'cleanup'))
        if name == 'inline_syntax_boundary':
            _inline_evidence(steps)
        scenarios.append({'name': name, 'result': 'pass', 'steps': steps})
    return {'request_id': REQUEST['request_id'], 'run_id': '123', 'run_attempt': '1',
            'procedure_version': policy.PROCEDURE, 'control': {'sha': CONTROL},
            'target': {'actual_sha': SHA, 'expected_sha': SHA, 'sha_matches': True, 'working_copy_clean': True},
            'started_at': '2026-09-09T00:01:00Z', 'finished_at': '2026-09-09T00:10:00Z',
            'build': {'binary_sha256': 'c' * 64, 'features': ['timing-probe'],
                      'source_snapshot_sha': SHA, 'source_snapshot_clean': True},
            'runner': {'os': 'macOS', 'arch': 'ARM64', 'machine': 'arm64', 'image_os': 'macos15',
                       'image_version': '20260829.0321.1', 'macos_version': '15.7.9'},
            'top_level_steps': [{'name': s, 'result': 'pass'} for s in ('preflight', 'prepare_helper', 'build')],
            'scenarios': scenarios, 'overall_result': 'pass'}


class ReceiptTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.evidence = Path(self.temp.name)
        for name in policy.REQUIRED_IMAGES:
            path = self.evidence / name
            path.parent.mkdir(parents=True, exist_ok=True)
            if name.endswith('_unclosed.body.png'):
                data = BODY_UNCLOSED_IMAGE
            elif name.endswith('.body.png'):
                data = BODY_CLOSED_IMAGE
            else:
                data = DEFAULT_IMAGE
            path.write_bytes(data)

    def validate(self, raw, conclusion='success'):
        return policy.validate_receipt(raw, REQUEST, self.evidence, conclusion, NOW)

    def inline_step(self, raw, name):
        inline = next(s for s in raw['scenarios'] if s['name'] == 'inline_syntax_boundary')
        return next(s for s in inline['steps'] if s['name'] == name)

    def test_all_three_terminal_results_are_preserved_for_final_judge(self):
        for result in ('pass', 'fail', 'blocked'):
            raw = passing_result()
            raw['overall_result'] = result
            with self.subTest(result=result):
                self.assertEqual(self.validate(raw, 'success' if result == 'pass' else 'failure'), result)

    def test_foreign_sha_generation_procedure_or_request_cannot_pass(self):
        mutations = [lambda r: r['target'].update(actual_sha='d' * 40),
                     lambda r: r.update(run_attempt='2'), lambda r: r.update(run_id='456'),
                     lambda r: r.update(request_id='other'), lambda r: r['control'].update(sha='e' * 40),
                     lambda r: r.update(procedure_version='launch-only'),
                     lambda r: r['target'].update(working_copy_clean=False)]
        for change in mutations:
            raw = passing_result()
            change(raw)
            with self.assertRaises(ValueError):
                self.validate(raw)

    def test_missing_runner_image_or_wrong_snapshot_cannot_pass(self):
        changes = [lambda r: r.pop('runner'), lambda r: r['runner'].pop('image_version'),
                   lambda r: r['runner'].update(arch='X64'),
                   lambda r: r['build'].update(source_snapshot_sha='d' * 40),
                   lambda r: r['build'].update(source_snapshot_clean=False)]
        for change in changes:
            raw = passing_result()
            change(raw)
            with self.assertRaises(ValueError):
                self.validate(raw)
        request = dict(REQUEST, procedure_version='hosted-gui-interaction/2')
        with self.assertRaises(ValueError):
            policy.validate_receipt(passing_result(), request, self.evidence, 'success', NOW)

    def test_missing_or_failed_scenario_step_and_reopen_cleanup_cannot_pass(self):
        for mutation in ('scenario', 'step', 'failure', 'cleanup', 'duplicate'):
            raw = passing_result()
            if mutation == 'scenario':
                raw['scenarios'].pop()
            elif mutation == 'step':
                raw['scenarios'][0]['steps'].pop(0)
            elif mutation == 'failure':
                raw['scenarios'][0]['steps'][0]['result'] = 'fail'
            elif mutation == 'cleanup':
                steps = raw['scenarios'][0]['steps']
                steps.pop(next(i for i, step in enumerate(steps) if step['name'] == 'cleanup'))
            else:
                raw['scenarios'].append(deepcopy(raw['scenarios'][0]))
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                self.validate(raw)

    def test_missing_screenshot_and_unsuccessful_worker_cannot_pass(self):
        for conclusion in ('failure', 'cancelled', 'timed_out', None):
            with self.assertRaises(ValueError):
                self.validate(passing_result(), conclusion)
        (self.evidence / policy.REQUIRED_IMAGES[0]).unlink()
        with self.assertRaises(ValueError):
            self.validate(passing_result())

    def test_dropping_the_inline_syntax_scenario_from_a_receipt_cannot_pass(self):
        raw = passing_result()
        raw['scenarios'] = [s for s in raw['scenarios'] if s['name'] != 'inline_syntax_boundary']
        with self.assertRaises(ValueError):
            self.validate(raw)

    def test_inline_operation_evidence_is_fail_closed(self):
        raw = passing_result()
        self.inline_step(raw, 'boundary_ime_input_check').pop('actual_after_insert')
        with self.assertRaises(ValueError):
            self.validate(raw)

    def test_inline_screenshot_refs_are_bound_to_required_images(self):
        raw = passing_result()
        self.inline_step(raw, 'boundary_click_edit_bold_italic_check_0')['screenshot'] = (
            'inline_syntax_boundary/boundary_click_edit_bold_italic_check_0.png'
        )
        with self.assertRaises(ValueError):
            self.validate(raw)

    def test_caret_navigation_evidence_is_fail_closed(self):
        for field, value in [('direction', 'right'), ('count', 2),
                             ('actual_after_move_insert', 'wrong source position')]:
            raw = passing_result()
            self.inline_step(raw, 'boundary_caret_navigation_check_0')[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.validate(raw)

    def test_delimiter_kind_and_visual_transition_are_fail_closed(self):
        mutations = [
            ('delimiter', '*'),
            ('initial_pixel_digest', '3' * 64),
            ('unclosed_pixel_digest', BODY_CLOSED_SHA),
            ('visual_transition_observed', False),
            ('closed_visual_restored', False),
        ]
        for field, value in mutations:
            raw = passing_result()
            self.inline_step(raw, 'delimiter_toggle_bold_check')[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.validate(raw)

    def test_delimiter_digest_must_match_staged_body_image(self):
        raw = passing_result()
        path = self.evidence / 'inline_syntax_boundary/delimiter_toggle_bold_unclosed.body.png'
        path.write_bytes(BODY_CLOSED_IMAGE)
        with self.assertRaises(ValueError):
            self.validate(raw)

    def test_delimiter_body_crop_is_required(self):
        raw = passing_result()
        path = self.evidence / 'inline_syntax_boundary/delimiter_toggle_code_closed.body.png'
        path.unlink()
        with self.assertRaises(ValueError):
            self.validate(raw)

    def test_delimiter_screenshot_refs_are_bound_to_state(self):
        raw = passing_result()
        step = self.inline_step(raw, 'delimiter_toggle_code_check')
        step['closed_screenshot'] = step['unclosed_screenshot']
        with self.assertRaises(ValueError):
            self.validate(raw)

    def test_multiline_code_span_toggle_visual_transition_is_fail_closed(self):
        mutations = [
            ('initial_pixel_digest', '3' * 64),
            ('unclosed_pixel_digest', BODY_CLOSED_SHA),
            ('visual_transition_observed', False),
            ('closed_visual_restored', False),
            ('unclosed_actual', policy.INLINE_FIXTURE_ORIGINAL),
            ('closed_actual', policy.INLINE_FIXTURE_ORIGINAL.replace('span`', 'span', 1)),
        ]
        for field, value in mutations:
            raw = passing_result()
            self.inline_step(raw, 'multiline_code_span_close_toggle_check')[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.validate(raw)

    def test_multiline_code_span_toggle_digest_must_match_staged_body_image(self):
        raw = passing_result()
        path = self.evidence / 'inline_syntax_boundary/multiline_code_span_close_toggle_unclosed.body.png'
        path.write_bytes(BODY_CLOSED_IMAGE)
        with self.assertRaises(ValueError):
            self.validate(raw)

    def test_duplicate_inline_evidence_step_cannot_pass(self):
        raw = passing_result()
        inline = next(s for s in raw['scenarios'] if s['name'] == 'inline_syntax_boundary')
        inline['steps'].append(deepcopy(self.inline_step(raw, 'boundary_ime_input_check')))
        with self.assertRaises(ValueError):
            self.validate(raw)

    def test_expired_or_reversed_result_rejected(self):
        for field, value in [('started_at', '2026-09-08T23:59:00Z'),
                             ('finished_at', '2026-09-09T01:01:00Z'),
                             ('finished_at', '2026-09-09T00:00:00Z')]:
            raw = passing_result()
            raw[field] = value
            with self.assertRaises(ValueError):
                self.validate(raw)


class RequirementAndReviewTests(unittest.TestCase):
    def test_only_complete_allowlisted_diff_can_skip_gui(self):
        self.assertFalse(policy.required_from_files([{'filename': 'docs/note.md'}], 1))
        self.assertTrue(policy.required_from_files([{'filename': 'docs/note.md'}], 1, True))
        self.assertTrue(policy.required_from_files([], 0))
        self.assertTrue(policy.required_from_files([{'filename': 'docs/a.md', 'previous_filename': 'crates/ui/a.rs'}], 1))
        for rows, count in [([{'filename': 'docs/a.md'}], 2), ([{}], 1), ([{'filename': None}], 1)]:
            with self.assertRaises(ValueError):
                policy.required_from_files(rows, count)

    def test_workflow_adapter_uses_the_same_rename_policy_as_resolver(self):
        cases = [([{'filename': 'docs/a.md', 'previous_filename': 'crates/ui/a.rs'}], 1, 'true'),
                 ([{'filename': 'docs/a.md', 'previous_filename': 'docs/old.md'}], 1, 'false'),
                 ([{'filename': 'docs/a.md', 'previous_filename': None}], 1, 'blocked'),
                 ([{'filename': 'docs/a.md'}], 2, 'blocked'), ([], 0, 'true')]
        for files, count, expected in cases:
            result = subprocess.run([sys.executable, '-I', str(SCRIPTS / 'gui_classify.py'), str(count)],
                                    input=json.dumps([files]), text=True, capture_output=True, check=True)
            self.assertEqual(result.stdout.strip(), expected)

    def test_head_label_or_policy_mismatch_cannot_reuse_classification(self):
        status = {'state': 'success', 'description': f'GUI validation not required (v1) for {SHA[:12]}'}
        self.assertTrue(policy.classification_matches(status, SHA, False))
        self.assertFalse(policy.classification_matches(status, SHA, True))
        self.assertFalse(policy.classification_matches(status, 'd' * 40, False))

    def test_findings_require_fresh_continue_validation(self):
        review = {'id': 10, 'state': 'failure', 'description': f'Codex findings for {SHA[:12]}'}
        route = {'id': 9, 'state': 'success', 'description': f'Copilot routing: continue-validation for {SHA[:12]}'}
        statuses = {'hane/codex-review': review, 'hane/copilot-routing': route}
        self.assertFalse(policy.review_ready(statuses, SHA))
        route['id'] = 11
        self.assertTrue(policy.review_ready(statuses, SHA))
        route['state'] = 'pending'
        self.assertFalse(policy.review_ready(statuses, SHA))

    def test_inline_syntax_boundary_required_steps_cover_representative_constructs(self):
        steps = policy.REQUIRED_STEPS['inline_syntax_boundary']
        for required in ('boundary_click_edit_bold_italic', 'boundary_click_edit_code_span',
                         'boundary_click_edit_quote', 'boundary_click_edit_list', 'boundary_caret_navigation',
                         'boundary_ime_input', 'drag_select_delete_undo_redo', 'delimiter_toggle_star',
                         'delimiter_toggle_bold', 'delimiter_toggle_code', 'reopen_content_check'):
            self.assertIn(required, steps)
        self.assertTrue(any(name.startswith('inline_syntax_boundary/') for name in policy.REQUIRED_IMAGES))
        self.assertTrue(any(name.endswith('.body.png') for name in policy.REQUIRED_IMAGES))

    def test_gui_status_requires_exact_sha_and_matching_terminal_state(self):
        row = {'state': 'success', 'description': f'GUI pass {policy.STATUS_VERSION} {SHA[:12]} g123-1'}
        self.assertEqual(policy.gui_state(row, SHA), ('pass', '123-1'))
        self.assertIsNone(policy.gui_state(row, 'd' * 40))
        row['state'] = 'error'
        self.assertIsNone(policy.gui_state(row, SHA))

    def test_fork_draft_unknown_author_and_non_main_base_do_not_execute(self):
        api = GitHub('owner/hane')
        pr = {'state': 'open', 'draft': False, 'head': {'sha': SHA, 'repo': {'full_name': 'owner/hane'}},
              'base': {'ref': 'main'}, 'user': {'login': 'owner'}}
        self.assertTrue(api.trusted(pr))
        for change in [lambda p: p['head'].update(repo=None), lambda p: p.update(draft=True),
                       lambda p: p['head']['repo'].update(full_name='outsider/fork'),
                       lambda p: p['user'].update(login='outsider'), lambda p: p['base'].update(ref='other')]:
            altered = deepcopy(pr)
            change(altered)
            self.assertFalse(api.trusted(altered))


if __name__ == '__main__':
    unittest.main()
