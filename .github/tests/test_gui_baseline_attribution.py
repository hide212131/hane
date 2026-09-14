"""GUI baseline attribution: exact-head/base/control/procedure/policy binding,
pre-existing-independent separation, and fail-closed unknown/regression/tamper
handling. No network, agents, builds, or GUI input."""
from pathlib import Path
import sys
import unittest
from unittest.mock import MagicMock, patch

SCRIPTS = Path(__file__).resolve().parents[1] / 'scripts'
sys.path.insert(0, str(SCRIPTS))
import gui_baseline_attribution as attribution
import gui_policy

SHA = 'a' * 40
BASE_SHA = 'b' * 40
ANCESTOR_SHA = 'e' * 40
CONTROL = 'c' * 40
REPO = 'owner/hane'


def raw_with(fail_top_level=(), fail_scenarios=()):
    return {
        'top_level_steps': [{'name': name, 'result': 'fail'} for name in fail_top_level],
        'scenarios': [
            {'name': name, 'result': 'fail',
             'steps': [{'name': step, 'result': 'fail'} for step in steps]}
            for name, steps in fail_scenarios
        ],
    }


def proof_for(sha, outcome, raw=None, control=CONTROL):
    return {'schema_version': 1, 'policy_version': 'v1',
            'request': {'control_sha': control, 'procedure_version': gui_policy.PROCEDURE, 'sha': sha},
            'outcome': outcome, 'result': raw}


class FailingUnitsTests(unittest.TestCase):
    def test_extracts_top_level_and_scenario_failures_only(self):
        raw = {
            'top_level_steps': [{'name': 'build', 'result': 'fail'}, {'name': 'preflight', 'result': 'pass'}],
            'scenarios': [
                {'name': 'os_scroll', 'result': 'fail',
                 'steps': [{'name': 'os_wheel', 'result': 'fail'}, {'name': 'capture_before', 'result': 'pass'}]},
                {'name': 'japanese_ime_input', 'result': 'pass', 'steps': []},
            ],
        }
        units = attribution.failing_units(raw)
        self.assertEqual(units, [
            {'kind': 'top_level', 'name': 'build', 'steps': ()},
            {'kind': 'scenario', 'name': 'os_scroll', 'steps': ('os_wheel',)},
        ])


class AttributeTests(unittest.TestCase):
    def test_pass_outcome_needs_no_attribution(self):
        result = attribution.attribute(proof_for(SHA, 'pass'), BASE_SHA, None)
        self.assertEqual(result, {'baseline': None, 'clusters': [], 'blocking': True})

    def test_blocked_outcome_never_waives_even_with_a_baseline(self):
        current = proof_for(SHA, 'blocked')
        baseline = {'sha': BASE_SHA, 'source': 'base-sha', 'receipt': proof_for(BASE_SHA, 'blocked')}
        result = attribution.attribute(current, BASE_SHA, baseline)
        self.assertTrue(result['blocking'])
        self.assertEqual(result['clusters'], [])

    def test_target_acceptance_scenario_always_blocks_even_with_identical_baseline(self):
        raw = raw_with(fail_scenarios=[('coordinate_independent_probe', ('coordinate_independent_probe',))])
        current = proof_for(SHA, 'fail', raw)
        baseline_receipt = proof_for(BASE_SHA, 'fail', raw)
        baseline = {'sha': BASE_SHA, 'source': 'base-sha', 'receipt': baseline_receipt}
        result = attribution.attribute(current, BASE_SHA, baseline)
        self.assertTrue(result['blocking'])
        self.assertEqual(result['clusters'][0]['classification'], 'target_acceptance')

    def test_identical_baseline_signature_is_pre_existing_independent(self):
        raw = raw_with(fail_scenarios=[('os_scroll', ('os_wheel',))])
        current = proof_for(SHA, 'fail', raw)
        baseline = {'sha': BASE_SHA, 'source': 'base-sha', 'receipt': proof_for(BASE_SHA, 'fail', raw)}
        result = attribution.attribute(current, BASE_SHA, baseline)
        self.assertFalse(result['blocking'])
        self.assertEqual(result['clusters'][0]['classification'], 'pre_existing_independent')

    def test_missing_baseline_is_unknown_fail_closed(self):
        raw = raw_with(fail_scenarios=[('os_scroll', ('os_wheel',))])
        result = attribution.attribute(proof_for(SHA, 'fail', raw), BASE_SHA, None)
        self.assertTrue(result['blocking'])
        self.assertEqual(result['clusters'][0]['classification'], 'unknown')

    def test_control_sha_mismatch_is_unknown_not_pre_existing(self):
        raw = raw_with(fail_scenarios=[('os_scroll', ('os_wheel',))])
        current = proof_for(SHA, 'fail', raw, control=CONTROL)
        baseline = {'sha': BASE_SHA, 'source': 'base-sha', 'receipt': proof_for(BASE_SHA, 'fail', raw, control='d' * 40)}
        result = attribution.attribute(current, BASE_SHA, baseline)
        self.assertTrue(result['blocking'])
        self.assertEqual(result['clusters'][0]['classification'], 'unknown')

    def test_procedure_mismatch_is_unknown_not_pre_existing(self):
        raw = raw_with(fail_scenarios=[('os_scroll', ('os_wheel',))])
        current = proof_for(SHA, 'fail', raw)
        stale_baseline = proof_for(BASE_SHA, 'fail', raw)
        stale_baseline['request']['procedure_version'] = 'hosted-gui-interaction/2'
        baseline = {'sha': BASE_SHA, 'source': 'base-sha', 'receipt': stale_baseline}
        result = attribution.attribute(current, BASE_SHA, baseline)
        self.assertEqual(result['clusters'][0]['classification'], 'unknown')

    def test_baseline_that_did_not_fail_that_scenario_is_a_regression(self):
        raw = raw_with(fail_scenarios=[('os_scroll', ('os_wheel',))])
        current = proof_for(SHA, 'fail', raw)
        baseline = {'sha': BASE_SHA, 'source': 'base-sha', 'receipt': proof_for(BASE_SHA, 'fail', raw_with())}
        result = attribution.attribute(current, BASE_SHA, baseline)
        self.assertTrue(result['blocking'])
        self.assertEqual(result['clusters'][0]['classification'], 'regression')

    def test_baseline_never_executed_that_scenario_is_a_regression_not_pre_existing(self):
        # Regression test for PR #132: a scenario baseline never ran (e.g.
        # drag/delimiter steps not yet in the baseline procedure) must not be
        # treated as pre-existing just because *some* baseline receipt exists.
        raw = raw_with(fail_scenarios=[('inline_syntax_boundary', ('drag_select_delete_undo_redo_check',))])
        current = proof_for(SHA, 'fail', raw)
        baseline = {'sha': BASE_SHA, 'source': 'base-sha', 'receipt': proof_for(BASE_SHA, 'fail', raw_with())}
        result = attribution.attribute(current, BASE_SHA, baseline)
        self.assertEqual(result['clusters'][0]['classification'], 'regression')

    def test_changed_symptom_is_unknown_not_pre_existing(self):
        # Regression test for PR #132: IME failing on a different step at
        # baseline than at the current head must not be waived as pre-existing.
        current_raw = raw_with(fail_scenarios=[('japanese_ime_input', ('ime_input_save',))])
        baseline_raw = raw_with(fail_scenarios=[('japanese_ime_input', ('capture_after',))])
        current = proof_for(SHA, 'fail', current_raw)
        baseline = {'sha': BASE_SHA, 'source': 'base-sha', 'receipt': proof_for(BASE_SHA, 'fail', baseline_raw)}
        result = attribution.attribute(current, BASE_SHA, baseline)
        self.assertTrue(result['blocking'])
        self.assertEqual(result['clusters'][0]['classification'], 'unknown')

    def test_baseline_anchored_to_a_different_sha_than_the_declared_base_is_rejected(self):
        # A baseline dict claiming 'base-sha' but pointing at a SHA that is not
        # actually the current Pull Request's base must not be trusted.
        raw = raw_with(fail_scenarios=[('os_scroll', ('os_wheel',))])
        current = proof_for(SHA, 'fail', raw)
        wrong_sha = 'f' * 40
        baseline = {'sha': wrong_sha, 'source': 'base-sha', 'receipt': proof_for(wrong_sha, 'fail', raw)}
        result = attribution.attribute(current, BASE_SHA, baseline)
        self.assertEqual(result['clusters'][0]['classification'], 'unknown')

    def test_controller_confirmed_equivalent_baseline_can_still_waive(self):
        raw = raw_with(fail_scenarios=[('os_scroll', ('os_wheel',))])
        current = proof_for(SHA, 'fail', raw)
        baseline = {'sha': ANCESTOR_SHA, 'source': 'controller-confirmed-equivalent',
                    'receipt': proof_for(ANCESTOR_SHA, 'fail', raw)}
        result = attribution.attribute(current, BASE_SHA, baseline)
        self.assertFalse(result['blocking'])
        self.assertEqual(result['clusters'][0]['classification'], 'pre_existing_independent')

    def test_baseline_outcome_blocked_cannot_authenticate_pre_existing(self):
        raw = raw_with(fail_scenarios=[('os_scroll', ('os_wheel',))])
        current = proof_for(SHA, 'fail', raw)
        baseline = {'sha': BASE_SHA, 'source': 'base-sha', 'receipt': proof_for(BASE_SHA, 'blocked', raw)}
        result = attribution.attribute(current, BASE_SHA, baseline)
        self.assertEqual(result['clusters'][0]['classification'], 'unknown')

    def test_mixed_units_only_waive_the_ones_that_fully_match(self):
        raw = raw_with(fail_scenarios=[('os_scroll', ('os_wheel',)), ('japanese_ime_input', ('ime_input_save',))])
        baseline_raw = raw_with(fail_scenarios=[('os_scroll', ('os_wheel',))])
        current = proof_for(SHA, 'fail', raw)
        baseline = {'sha': BASE_SHA, 'source': 'base-sha', 'receipt': proof_for(BASE_SHA, 'fail', baseline_raw)}
        result = attribution.attribute(current, BASE_SHA, baseline)
        self.assertTrue(result['blocking'])
        by_name = {c['name']: c['classification'] for c in result['clusters']}
        self.assertEqual(by_name['os_scroll'], 'pre_existing_independent')
        self.assertEqual(by_name['japanese_ime_input'], 'regression')


class BaselineReceiptTests(unittest.TestCase):
    def make_api(self, status_row, run, jobs):
        api = MagicMock(repository=REPO)
        api.repo.side_effect = lambda suffix: suffix
        api.statuses.return_value = {gui_policy.CONTEXT: status_row} if status_row else {}
        api.api.return_value = run
        api.pages.return_value = jobs
        return api

    def status_row(self, outcome, run_id='123', attempt='1'):
        return {'state': {'pass': 'success', 'fail': 'failure', 'blocked': 'error'}[outcome],
                'description': f'GUI {outcome} {gui_policy.STATUS_VERSION} {BASE_SHA[:12]} g{run_id}-{attempt}',
                'target_url': f'https://github.com/{REPO}/actions/runs/{run_id}'}

    def test_no_terminal_status_returns_none(self):
        api = self.make_api(None, {}, [])
        self.assertIsNone(attribution.baseline_gui_receipt(api, BASE_SHA))

    def test_pending_status_returns_none(self):
        row = dict(self.status_row('pass'), state='pending',
                   description=f'GUI pending {gui_policy.STATUS_VERSION} {BASE_SHA[:12]} g123-1')
        api = self.make_api(row, {}, [])
        self.assertIsNone(attribution.baseline_gui_receipt(api, BASE_SHA))

    def test_status_url_mismatch_is_rejected(self):
        row = dict(self.status_row('pass'), target_url='https://github.com/owner/other/actions/runs/123')
        api = self.make_api(row, {}, [])
        with self.assertRaises(ValueError):
            attribution.baseline_gui_receipt(api, BASE_SHA)

    def test_run_not_completed_returns_none(self):
        api = self.make_api(self.status_row('pass'), {'status': 'in_progress'}, [])
        self.assertIsNone(attribution.baseline_gui_receipt(api, BASE_SHA))

    def test_no_gui_worker_job_name_returns_none(self):
        api = self.make_api(self.status_row('pass'), {'status': 'completed'}, [{'name': 'something else'}])
        self.assertIsNone(attribution.baseline_gui_receipt(api, BASE_SHA))

    def test_recovers_pr_number_from_job_name_and_authenticates(self):
        run = {'id': 123, 'run_attempt': 1, 'status': 'completed', 'path': '.github/workflows/gui-validation.yml',
               'head_branch': 'main', 'head_sha': CONTROL, 'event': 'workflow_dispatch'}
        api = self.make_api(self.status_row('fail'), run, [{'name': 'GUI worker #42'}])
        proof = proof_for(BASE_SHA, 'fail')
        proof['request'].update(pr_number=42, sha=BASE_SHA, repository=REPO, generation='123-1',
                                run_id='123', run_attempt='1', request_id='gui-123-1-pr42', control_sha=CONTROL)
        with patch.object(attribution, 'artifact_json', return_value=proof) as fetch:
            result = attribution.baseline_gui_receipt(api, BASE_SHA)
        fetch.assert_called_once_with(api, '123', 'gui-receipt-42-123-1', 'gui-receipt.json')
        self.assertEqual(result, proof)


class ProductEquivalentBaselineTests(unittest.TestCase):
    def make_api(self, commits, compare_files_by_sha):
        api = MagicMock(repository=REPO)
        api.repo.side_effect = lambda suffix: suffix

        def pages(suffix, key=None):
            if suffix.startswith('commits?sha='):
                return commits
            raise AssertionError(f'unexpected pages() call: {suffix}')
        api.pages.side_effect = pages

        def call(suffix, body=None, method=None):
            if suffix.startswith('compare/'):
                sha = suffix.split('/')[1].split('...')[0]
                return {'files': compare_files_by_sha[sha]}
            raise AssertionError(f'unexpected api() call: {suffix}')
        api.api.side_effect = call
        return api

    def test_no_older_commit_has_a_receipt(self):
        commits = [{'sha': BASE_SHA}, {'sha': ANCESTOR_SHA}]
        api = self.make_api(commits, {})
        with patch.object(attribution, 'baseline_gui_receipt', return_value=None):
            self.assertIsNone(attribution.product_equivalent_baseline(api, BASE_SHA))

    def test_unrelated_file_changes_are_not_product_equivalent(self):
        commits = [{'sha': BASE_SHA}, {'sha': ANCESTOR_SHA}]
        api = self.make_api(commits, {ANCESTOR_SHA: [{'filename': 'crates/editor/src/lib.rs'}]})
        with patch.object(attribution, 'baseline_gui_receipt', return_value=proof_for(ANCESTOR_SHA, 'fail')):
            self.assertIsNone(attribution.product_equivalent_baseline(api, BASE_SHA))

    def test_docs_only_changes_are_product_equivalent(self):
        commits = [{'sha': BASE_SHA}, {'sha': ANCESTOR_SHA}]
        api = self.make_api(commits, {ANCESTOR_SHA: [{'filename': 'docs/notes.md'}]})
        receipt = proof_for(ANCESTOR_SHA, 'fail')
        with patch.object(attribution, 'baseline_gui_receipt', return_value=receipt):
            result = attribution.product_equivalent_baseline(api, BASE_SHA)
        self.assertEqual(result, {'sha': ANCESTOR_SHA, 'receipt': receipt, 'source': 'controller-confirmed-equivalent'})

    def test_identical_tree_with_zero_changed_files_is_product_equivalent(self):
        commits = [{'sha': BASE_SHA}, {'sha': ANCESTOR_SHA}]
        api = self.make_api(commits, {ANCESTOR_SHA: []})
        receipt = proof_for(ANCESTOR_SHA, 'fail')
        with patch.object(attribution, 'baseline_gui_receipt', return_value=receipt):
            result = attribution.product_equivalent_baseline(api, BASE_SHA)
        self.assertEqual(result['sha'], ANCESTOR_SHA)

    def test_omitted_or_truncated_diff_is_fail_closed(self):
        commits = [{'sha': BASE_SHA}, {'sha': ANCESTOR_SHA}]
        api = MagicMock(repository=REPO)
        api.repo.side_effect = lambda suffix: suffix
        api.pages.return_value = commits
        api.api.return_value = {}  # no 'files' key at all: ambiguous, large diff
        with patch.object(attribution, 'baseline_gui_receipt', return_value=proof_for(ANCESTOR_SHA, 'fail')):
            self.assertIsNone(attribution.product_equivalent_baseline(api, BASE_SHA))


if __name__ == '__main__':
    unittest.main()
