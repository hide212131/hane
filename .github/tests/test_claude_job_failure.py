import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('report_job_failure', ROOT / 'scripts/report_claude_job_failure.py')
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)
SHA = 'a' * 40
REPO = 'hide212131/hane'

class JobFailureTests(unittest.TestCase):
    def run_report(self, *, comments=(), stage='claude-fix', result='failure', trusted=True):
        writes = []
        def api(endpoint, payload=None):
            if payload is not None:
                writes.append((endpoint, payload))
                return {}
            if '/pulls/' in endpoint:
                return {'base': {'repo': {'full_name': REPO}}, 'head': {'repo': {'full_name': REPO}},
                        'user': {'login': 'hide212131' if trusted else 'untrusted'}}
            return list(comments)
        report.notify(api, repository=REPO, number='84', sha=SHA, stage=stage,
                      result=result, run_id='123', attempt='1')
        return writes

    def test_job_timeout_without_worker_outputs_posts_fallback(self):
        self.assertEqual(len(self.run_report()), 1)

    def test_cancelled_job_posts_fallback(self):
        self.assertEqual(len(self.run_report(result='cancelled')), 1)

    def test_success_and_skipped_are_quiet(self):
        for result in ('success', 'skipped'):
            self.assertEqual(self.run_report(result=result), [])

    def test_untrusted_pr_is_not_notified(self):
        self.assertEqual(self.run_report(trusted=False), [])

    def test_existing_detailed_notification_is_preserved(self):
        body = f'https://github.com/{REPO}/actions/runs/123/attempts/1\n<!-- hane-stall: number=84 stage=claude-fix sha={SHA} -->'
        self.assertEqual(self.run_report(comments=[{'id': 1, 'user': {'login': 'github-actions[bot]'}, 'body': body}]), [])

    def test_newer_execution_report_is_not_overwritten(self):
        body = f'https://github.com/{REPO}/actions/runs/124/attempts/1\n<!-- hane-stall: number=84 stage=claude-fix sha={SHA} -->'
        self.assertEqual(self.run_report(comments=[{'id': 1, 'user': {'login': 'github-actions[bot]'}, 'body': body}]), [])

    def test_human_marker_cannot_suppress_fallback(self):
        body = f'https://github.com/{REPO}/actions/runs/123/attempts/1\n<!-- hane-stall: number=84 stage=claude-fix sha={SHA} -->'
        self.assertEqual(len(self.run_report(comments=[{'id': 1, 'user': {'login': 'human'}, 'body': body}])), 1)

    def test_implementation_failure_targets_issue(self):
        writes = self.run_report(stage='implement')
        self.assertIn('Issue #84', writes[0][1]['body'])

    def test_notification_jobs_are_independent_and_trusted(self):
        for filename, worker in [('implement.yml', 'implement'), ('claude-fix.yml', 'fix')]:
            text = (ROOT / 'workflows' / filename).read_text().split('  report_job_failure:\n')[1]
            self.assertIn('always()', text)
            self.assertIn('needs.' + worker + '.result', text)
            self.assertIn('ref: ${{ github.workflow_sha }}', text)
            self.assertIn('TARGET_NUMBER:', text)
        implement = (ROOT / 'workflows/implement.yml').read_text().split('  report_job_failure:\n')[1]
        self.assertIn("needs.authorize.outputs.authorized == 'true'", implement)
