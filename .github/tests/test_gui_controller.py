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
        self.rows = {}
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
        return {'id': 123, 'status': self.run_status}

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
            artifacts = [{'name': 'gui-receipt-79-123-1', 'expired': False, 'size_in_bytes': 100}]
            with self.subTest(status=status), patch.object(self.api, 'pages', return_value=artifacts) as pages:
                controller.resolve(self.api)
            self.assertEqual(self.api.writes, [])
            self.assertEqual(pages.call_count, 1 if status == 'completed' else 0)

    def test_expired_request_cannot_begin(self):
        with patch.object(controller, 'now', return_value=NOW.replace(hour=2)):
            controller.begin(self.api, REQUEST)
        self.assertIn('proceed=false', (self.root / 'outputs').read_text())


class CITests(unittest.TestCase):
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
