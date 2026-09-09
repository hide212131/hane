"""Execute the trusted workflow notification step with a fake GitHub API."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

WORKFLOW = (Path(__file__).resolve().parents[1] / 'workflows/claude-fix.yml').read_text()
STEP = WORKFLOW.split('      - name: Report Claude fix outcome to the Pull Request\n')[1].split('\n  report_job_failure:', 1)[0]
SCRIPT = textwrap.dedent(STEP.split('        run: |\n')[1])
SHA = 'a' * 40

class FixNotificationsTests(unittest.TestCase):
    def run_report(self, description, comments=(), **extra):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake = root / 'gh'
            fake.write_text('#!' + sys.executable + '\n' + textwrap.dedent('''
                import json, os, sys
                from pathlib import Path
                args = sys.argv[1:]
                if '--method' in args:
                    with open(os.environ['WRITES'], 'a') as f:
                        f.write(json.dumps(args) + '\\n')
                    print('{}')
                elif any('/statuses?' in x for x in args):
                    print(json.dumps([[{'id': 1, 'context': 'hane/claude-fix', 'description': os.environ['DESCRIPTION']}]]))
                else:
                    print(json.dumps([json.loads(os.environ['COMMENTS'])]))
            '''))
            fake.chmod(0o755)
            writes = root / 'writes'
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ['PATH'],
                       REPOSITORY='hide212131/hane', PR_NUMBER='84', TARGET_SHA=SHA,
                       GITHUB_RUN_ID='123', GITHUB_RUN_ATTEMPT='1', CLAIMED='true', CLAUDE_EXIT='1', JOB_STATUS='success',
                       DESCRIPTION=description, COMMENTS=json.dumps(comments), WRITES=str(writes))
            env.update(extra)
            result = subprocess.run(['bash', '-c', SCRIPT], env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            return [json.loads(x) for x in writes.read_text().splitlines()] if writes.exists() else []

    def test_claude_timeout_reaches_status_and_notification_steps(self):
        step = WORKFLOW.split("      - name: Run Claude Code fix\n")[1].split("      - name:", 1)[0]
        self.assertIn("timeout-minutes: 45", step)
        self.assertIn("continue-on-error: true", step)
        self.assertIn("always()", STEP.split("        run: |", 1)[0])

    def test_failure_posts_japanese_diagnostic(self):
        writes = self.run_report('Claude fix failed for ' + SHA[:12])
        self.assertEqual(len(writes), 1)
        self.assertIn('POST', writes[0])
        self.assertIn('修正処理が失敗', writes[0][-1])

    def test_forged_human_marker_is_never_updated(self):
        marker = f'<!-- hane-stall: number=84 stage=claude-fix sha={SHA} -->'
        writes = self.run_report('Claude fix failed for ' + SHA[:12],
            [{'id': 99, 'user': {'login': 'human'}, 'body': marker}])
        self.assertIn('POST', writes[0])

    def test_completed_run_resolves_own_previous_comment(self):
        marker = f'<!-- hane-stall: number=84 stage=claude-fix sha={SHA} -->'
        writes = self.run_report('Claude fix completed for ' + SHA[:12],
            [{'id': 99, 'user': {'login': 'github-actions[bot]'}, 'body': marker}])
        self.assertIn('PATCH', writes[0])
        self.assertIn('解決済み', writes[0][-1])

    def test_success_without_prior_stall_is_quiet(self):
        self.assertEqual(self.run_report('Claude fix completed for ' + SHA[:12]), [])

    def test_unknown_status_text_is_never_published(self):
        self.assertEqual(self.run_report('untrusted SECRET'), [])

    def test_cancelled_claim_is_reported(self):
        writes = self.run_report('Claude fix running for ' + SHA[:12], JOB_STATUS='cancelled')
        self.assertIn('実行が中止', writes[0][-1])

    def test_preclaim_cancel_is_quiet(self):
        self.assertEqual(self.run_report('', JOB_STATUS='cancelled', CLAIMED='false'), [])
