"""Lifecycle regressions for stale receipts, lost workers, and CI gating."""
from copy import deepcopy
from pathlib import Path
import os
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import gui_pipeline as controller
from pipeline_api import GitHub, CI_NAMES
from test_gui_policy import SHA, REQUEST, NOW
from gui_policy import STATUS_VERSION


class FakeGitHub(GitHub):
    def __init__(self):
        super().__init__('owner/hane')
        self.pull = {'number': 79, 'state': 'open', 'draft': False,
                     'head': {'sha': SHA, 'repo': {'full_name': self.repository}},
                     'base': {'ref': 'main'}, 'user': {'login': 'owner'}}
        self.rows = {'hane/codex-review': {'state': 'success',
                     'description': f'Codex review clean for {SHA[:12]}'}}
        self.writes = []
        self.paths = []
        self.run_status = 'completed'

    def pr(self, number):
        return deepcopy(self.pull)

    def statuses(self, sha):
        return deepcopy(self.rows)

    def post_status(self, *args):
        self.writes.append(args)

    def api(self, path, **kwargs):
        self.paths.append(path)
        return {'id': 123, 'run_attempt': 1, 'status': self.run_status,
                'path': '.github/workflows/gui-validation.yml', 'head_branch': 'main',
                'head_sha': 'b' * 40, 'event': 'workflow_dispatch'}

    def evidence(self, pr):
        return dict.fromkeys(('gui_required', 'classified', 'review_ready', 'ci_ready'), True)


class ControllerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = patch.dict(os.environ, {'GITHUB_REPOSITORY': 'owner/hane',
            'RUNNER_TEMP': self.temp.name, 'GITHUB_OUTPUT': str(self.root / 'outputs'),
            'GITHUB_RUN_ID': '456', 'GITHUB_RUN_ATTEMPT': '1', 'CONTROL_SHA': 'b' * 40,
            'INPUT_PR': '79', 'INPUT_SHA': '', 'INPUT_FORCE': 'false'})
        self.env.start()
        self.addCleanup(self.env.stop)
        self.api = FakeGitHub()
        self.api.rows['hane/gui-validation'] = {'state': 'pending',
            'description': f'GUI pending {STATUS_VERSION} {SHA[:12]} g123-1',
            'created_at': '2026-09-09T00:10:00Z'}

    def test_waiting_review_skips_expensive_evidence_and_retires_pending(self):
        for pending in (False, True):
            with self.subTest(pending=pending):
                api = FakeGitHub()
                api.rows.clear()
                if pending:
                    api.rows[controller.CONTEXT] = self.api.rows[controller.CONTEXT]
                with patch.object(api, 'evidence', side_effect=AssertionError('unready review must not fetch CI/files')), \
                        patch.object(controller, 'now', return_value=NOW):
                    controller.resolve(api)
                self.assertEqual(len(api.writes), int(pending))
                if pending:
                    self.assertIn('GUI superseded', api.writes[0][3])

    def test_report_never_writes_after_head_or_generation_changes(self):
        for kind in ('head', 'generation'):
            with self.subTest(kind=kind):
                api = FakeGitHub()
                api.rows = deepcopy(self.api.rows)
                if kind == 'head':
                    api.pull['head']['sha'] = 'd' * 40
                else:
                    api.rows['hane/gui-validation']['description'] = f'GUI pending {STATUS_VERSION} {SHA[:12]} g999-1'
                controller.report(api, REQUEST)
                self.assertEqual(api.writes, [])

    def test_missing_artifact_becomes_blocked_receipt(self):
        with patch.dict(os.environ, {'EVIDENCE_DIR': str(self.root / 'missing'),
                                    'RECEIPT_PATH': str(self.root / 'receipt.json')}):
            controller.report(self.api, REQUEST)
        self.assertEqual(self.api.writes[0][2], 'error')
        self.assertTrue((self.root / 'receipt.json').is_file())

    def test_completed_pending_attempt_is_recovered_once_without_execution(self):
        with patch.object(controller, 'now', return_value=NOW):
            controller.resolve(self.api)
        self.assertIn('repos/owner/hane/actions/runs/123/attempts/1', self.api.paths)
        self.assertEqual(len(self.api.writes), 1)
        self.assertEqual(self.api.writes[0][2], 'error')
        self.assertIn('has_work=false', (self.root / 'outputs').read_text())
        self.assertTrue((self.root / 'gui-recovered/79.json').is_file())

    def test_live_pending_attempt_is_not_replaced(self):
        self.api.run_status = 'in_progress'
        with patch.object(controller, 'now', return_value=NOW):
            controller.resolve(self.api)
        self.assertEqual(self.api.writes, [])
        self.assertIn('has_work=false', (self.root / 'outputs').read_text())

    def test_obsolete_procedure_terminal_status_starts_fresh_generation(self):
        self.api.rows['hane/gui-validation'] = {'state': 'success',
            'description': f'GUI pass v1 {SHA[:12]} g123-1'}
        with patch.object(controller, 'now', return_value=NOW):
            controller.resolve(self.api)
        self.assertEqual(len(self.api.writes), 1)
        self.assertEqual(self.api.writes[0][2], 'pending')
        self.assertIn(STATUS_VERSION, self.api.writes[0][3])
        self.assertIn('g456-1', self.api.writes[0][3])
        self.assertIn('has_work=true', (self.root / 'outputs').read_text())

    def test_missing_or_expired_terminal_receipt_starts_fresh_generation(self):
        for artifacts in ([], [{'name': 'gui-receipt-79-123-1', 'expired': True, 'size_in_bytes': 100}]):
            self.api.writes.clear()
            self.api.rows['hane/gui-validation'] = {'state': 'success',
                'description': f'GUI pass {STATUS_VERSION} {SHA[:12]} g123-1'}
            with self.subTest(artifacts=artifacts), patch.object(controller, 'now', return_value=NOW), \
                    patch.object(self.api, 'pages', return_value=artifacts):
                controller.resolve(self.api)
            self.assertEqual(len(self.api.writes), 1)
            self.assertEqual(self.api.writes[0][2], 'pending')
            self.assertIn('g456-1', self.api.writes[0][3])
            self.assertIn('replaces_generation', (self.root / 'outputs').read_text())

    def test_retained_receipt_or_incomplete_upload_does_not_repeat_gui(self):
        for status in ('completed', 'in_progress'):
            self.api.run_status = status
            self.api.rows['hane/gui-validation'] = {'state': 'success',
                'description': f'GUI pass {STATUS_VERSION} {SHA[:12]} g123-1'}
            proof = {'schema_version': 1, 'policy_version': 'v1', 'request': REQUEST, 'outcome': 'pass'}
            with self.subTest(status=status), patch.object(controller, 'artifact_json', return_value=proof) as read:
                controller.resolve(self.api)
            self.assertEqual(self.api.writes, [])
            self.assertEqual(read.call_count, 1 if status == 'completed' else 0)

    def test_recovery_receipt_is_authenticated_for_the_requested_pr(self):
        proof = {'schema_version': 1, 'policy_version': 'v1', 'request': REQUEST, 'outcome': 'blocked'}
        other = deepcopy(proof)
        other['request'].update(pr_number=80, request_id='gui-123-1-pr80')
        for result, expected in ((proof, True), (other, False), (ValueError('missing 79.json member'), False)):
            with self.subTest(result=result), patch.object(controller, 'artifact_json', side_effect=[ValueError('normal missing'), result]) as read:
                self.assertEqual(controller.terminal_receipt_retained(self.api, self.api.pull, ('blocked', '123-1')), expected)
                self.assertEqual(read.call_args.args[-1], '79.json')
        with patch.object(controller, 'artifact_json', return_value=proof) as read:
            self.assertTrue(controller.terminal_receipt_retained(self.api, self.api.pull, ('blocked', '123-1')))
            self.assertEqual(read.call_count, 1)  # A different PR's recovery artifact is irrelevant.

    def test_shared_recovery_archive_requires_the_exact_pr_member(self):
        import io
        import json
        import subprocess
        import zipfile
        import gui_artifacts
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, 'w') as zipped:
            zipped.writestr('80.json', json.dumps({'request': {'pr_number': 80}}))
        rows = [{'id': 1, 'name': 'gui-recovered-123-1', 'expired': False, 'size_in_bytes': len(archive.getvalue())}]
        response = subprocess.CompletedProcess([], 0, archive.getvalue(), b'')
        with patch.object(self.api, 'pages', return_value=rows), \
                patch.object(gui_artifacts.subprocess, 'run', return_value=response):
            with self.assertRaises(ValueError):
                gui_artifacts.artifact_json(self.api, '123', 'gui-recovered-123-1', '79.json')
            self.assertEqual(gui_artifacts.artifact_json(self.api, '123', 'gui-recovered-123-1', '80.json')['request']['pr_number'], 80)

    def test_expired_request_cannot_begin(self):
        with patch.object(controller, 'now', return_value=NOW.replace(hour=2)):
            controller.begin(self.api, REQUEST)
        self.assertIn('proceed=false', (self.root / 'outputs').read_text())

    def test_lost_eligibility_retires_pending_without_gui_failure(self):
        for field in ('gui_required', 'classified', 'review_ready', 'ci_ready'):
            for mode in ('begin', 'report', 'resolve'):
                self.api.writes.clear()
                data = self.api.evidence(self.api.pull)
                data[field] = False
                with self.subTest(field=field, mode=mode), \
                        patch.object(self.api, 'evidence', return_value=data), \
                        patch.object(controller, 'now', return_value=NOW):
                    if mode == 'resolve':
                        controller.resolve(self.api)
                    else:
                        getattr(controller, mode)(self.api, REQUEST)
                    if mode == 'begin':
                        self.assertEqual(self.api.writes, [])
                        controller.resolve(self.api)
                self.assertEqual(len(self.api.writes), 1)
                self.assertIn('GUI superseded', self.api.writes[0][3])
                self.api.rows['hane/gui-validation']['description'] = self.api.writes[0][3]
                self.api.writes.clear()
                with patch.object(controller, 'now', return_value=NOW):
                    controller.resolve(self.api)
                self.assertEqual(self.api.writes[0][2], 'pending')
                self.api.rows['hane/gui-validation']['description'] = f'GUI pending {STATUS_VERSION} {SHA[:12]} g123-1'


class CITests(unittest.TestCase):
    def test_reported_check_details_are_the_exact_records_used_by_gate(self):
        api = GitHub('owner/hane')
        checks = [{'id': i + 1, 'name': name, 'head_sha': SHA, 'app': {'slug': 'github-actions'},
                   'status': 'completed', 'conclusion': 'success', 'html_url': f'check/{i + 1}'}
                  for i, name in enumerate(CI_NAMES)]
        checks += [dict(checks[0], id=99, head_sha='b' * 40, conclusion='failure'),
                   dict(checks[1], id=100, app={'slug': 'other-app'}, conclusion='failure')]
        run = {'id': 10, 'head_sha': SHA, 'path': '.github/workflows/ci.yml',
               'status': 'completed', 'conclusion': 'success', 'html_url': 'run/10'}
        with patch.object(api, 'pages', return_value=checks), patch.object(api, 'api', return_value={'workflow_runs': [run]}):
            proof = api.ci_evidence(SHA, {})
            self.assertTrue(proof['ready'])
            self.assertEqual([c['id'] for c in proof['checks']], [1, 2])
            self.assertEqual(proof['workflow']['html_url'], 'run/10')
            run['conclusion'] = 'failure'
            proof = api.ci_evidence(SHA, {})
            self.assertFalse(proof['ready'])
            self.assertTrue(all(c['conclusion'] == 'success' for c in proof['checks']))
            self.assertEqual(proof['workflow']['conclusion'], 'failure')

    def test_generation_mismatch_is_visible_in_ci_details(self):
        api = GitHub('owner/hane')
        statuses = {name: {'state': 'success', 'target_url': f'run/{i + 1}'}
                    for i, name in enumerate(CI_NAMES)}
        statuses['hane/trusted-ci-generation'] = {'state': 'success',
            'description': 'Trusted CI generation 1-1 passed', 'target_url': 'run/1'}
        proof = api.ci_evidence(SHA, statuses)
        self.assertFalse(proof['ready'])
        self.assertEqual(proof['generation']['target_url'], 'run/1')
        self.assertEqual([c['target_url'] for c in proof['checks']], ['run/1', 'run/2'])

    def test_latest_platform_and_whole_workflow_must_pass(self):
        api = GitHub('owner/hane')
        checks = [{'id': i, 'name': name, 'head_sha': SHA, 'app': {'slug': 'github-actions'},
                   'status': 'completed', 'conclusion': 'success'} for i, name in enumerate(CI_NAMES)]
        run = {'id': 10, 'path': '.github/workflows/ci.yml', 'status': 'completed', 'conclusion': 'success'}
        with patch.object(api, 'pages', return_value=checks), patch.object(api, 'api', return_value={'workflow_runs': [run]}):
            self.assertTrue(api.ci_ready(SHA, {}))
            for conclusion in ('failure', 'skipped', None):
                checks.append(dict(checks[0], id=99, conclusion=conclusion))
                self.assertFalse(api.ci_ready(SHA, {}))
                checks.pop()
            run['conclusion'] = 'failure'
            self.assertFalse(api.ci_ready(SHA, {}))
            run['conclusion'] = 'success'
            checks.pop()
            self.assertFalse(api.ci_ready(SHA, {}))

    def test_trusted_generation_does_not_mix_platform_runs(self):
        api = GitHub('owner/hane')
        statuses = {name: {'state': 'success', 'target_url': 'run/1'} for name in CI_NAMES}
        statuses['hane/trusted-ci-generation'] = {'state': 'success',
            'description': 'Trusted CI generation 1-1 passed', 'target_url': 'run/1'}
        self.assertTrue(api.ci_ready(SHA, statuses))
        statuses[CI_NAMES[1]]['target_url'] = 'run/2'
        self.assertFalse(api.ci_ready(SHA, statuses))


if __name__ == '__main__':
    unittest.main()
