"""GUI receipt boundary regressions; no network, agents, builds, or GUI input."""
from copy import deepcopy
from datetime import datetime, timezone
import importlib.util
import json
import os
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


def passing_result():
    scenarios = []
    for name, expected in policy.REQUIRED_STEPS.items():
        steps = [{'name': step, 'result': 'pass'} for step in sorted(expected)]
        if name == 'ascii_edit_save_undo_redo_reopen':
            steps.extend({'name': step, 'result': 'pass'} for step in ('launch', 'window_discovery', 'cleanup'))
        scenarios.append({'name': name, 'result': 'pass', 'steps': steps})
    return {'request_id': REQUEST['request_id'], 'run_id': '123', 'run_attempt': '1',
            'procedure_version': policy.PROCEDURE, 'control': {'sha': CONTROL},
            'target': {'actual_sha': SHA, 'expected_sha': SHA, 'sha_matches': True, 'working_copy_clean': True},
            'started_at': '2026-09-09T00:01:00Z', 'finished_at': '2026-09-09T00:10:00Z',
            'build': {'binary_sha256': 'c' * 64, 'features': ['timing-probe'],
                      'source_snapshot_sha': SHA, 'source_snapshot_clean': True},
            'runner': {'os': 'macOS', 'arch': 'ARM64', 'machine': 'arm64', 'image_os': 'macos15',
                       'image_version': '20260829.0321.1', 'macos_version': '15.7.9'},
            'top_level_steps': [{'name': s, 'result': 'pass'} for s in ('preflight', 'build')],
            'scenarios': scenarios, 'overall_result': 'pass'}


class ReceiptTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.evidence = Path(self.temp.name)
        for name in policy.REQUIRED_IMAGES:
            path = self.evidence / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'\x89PNG\r\n\x1a\n' + b'x' * 200)

    def validate(self, raw, conclusion='success'):
        return policy.validate_receipt(raw, REQUEST, self.evidence, conclusion, NOW)

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

    def test_gui_status_requires_exact_sha_and_matching_terminal_state(self):
        row = {'state': 'success', 'description': f'GUI pass v1 {SHA[:12]} g123-1'}
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
