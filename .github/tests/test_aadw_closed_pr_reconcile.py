import datetime as dt
import re
import sys
from pathlib import Path
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import aadw_closed_pr_reconcile as subject
import aadw_prior_head_reconcile as prior

REPO = 'hide212131/hane'
SHA = 'a' * 40
NOW = dt.datetime(2026, 9, 12, 3, 0, tzinfo=dt.timezone.utc)
MARKER = (
    f'<!-- hane-aadw: process=codex-review kind=pr number=42 '
    f'sha={SHA} run=700 attempt=2 -->'
)


def start_comment(*, ident=1, actor='github-actions[bot]', created='2026-09-12T02:55:00Z'):
    return {
        'id': ident,
        'user': {'login': actor},
        'created_at': created,
        'updated_at': created,
        'body': '### AADW: Codexレビュー — 処理開始\n\n' + MARKER + '\n',
    }


class Fake:
    def __init__(self, *, comments=None, state='closed', run_status='in_progress', conclusion=None):
        self.comments = list(comments or [])
        self.state = state
        self.run_status = run_status
        self.conclusion = conclusion
        self.calls = []

    def call(self, endpoint, payload=None, method=None):
        self.calls.append((endpoint, payload, method))
        clean = endpoint.split('?', 1)[0]
        if clean == f'repos/{REPO}/issues/comments':
            return list(self.comments)
        if clean == f'repos/{REPO}/pulls/42':
            return {
                'number': 42,
                'state': self.state,
                'draft': False,
                'user': {'login': 'github-actions[bot]'},
                'head': {'sha': SHA, 'repo': {'full_name': REPO}},
            }
        if clean == f'repos/{REPO}/commits/{SHA}/statuses':
            return []
        if clean == f'repos/{REPO}/issues/42/comments':
            return list(self.comments)
        if clean == f'repos/{REPO}/actions/runs/700/attempts/2':
            return {
                'run_attempt': 2,
                'status': self.run_status,
                'conclusion': self.conclusion,
            }
        match = re.fullmatch(rf'repos/{re.escape(REPO)}/issues/comments/([1-9][0-9]*)', clean)
        if match:
            ident = int(match.group(1))
            row = next(item for item in self.comments if item['id'] == ident)
            if method == 'PATCH':
                row['body'] = payload['body']
                return row
            if method == 'DELETE':
                self.comments.remove(row)
                return None
            return dict(row)
        raise AssertionError(endpoint)


class Tests(unittest.TestCase):
    def test_recent_trusted_start_nominates_closed_pr_without_scanning_closed_list(self):
        fake = Fake(comments=[start_comment()])
        with patch.object(prior, 'reconcile_sha', return_value=0) as recover:
            self.assertEqual(subject.reconcile(fake.call, REPO, now=NOW), 0)
        recover.assert_called_once()
        endpoints = [call[0] for call in fake.calls]
        self.assertIn(f'repos/{REPO}/pulls/42', endpoints)
        self.assertFalse(any('pulls?state=closed' in endpoint for endpoint in endpoints))

    def test_completed_run_without_terminal_status_closes_start_as_abnormal(self):
        fake = Fake(comments=[start_comment()], run_status='completed', conclusion='cancelled')
        with patch.object(prior, 'reconcile_sha', return_value=0):
            self.assertEqual(subject.reconcile(fake.call, REPO, now=NOW), 1)
        self.assertIn('異常終了', fake.comments[0]['body'])
        self.assertIn('cancelled', fake.comments[0]['body'])
        self.assertIn('run=700 attempt=2', fake.comments[0]['body'])

    def test_recovered_terminal_is_not_overwritten_by_completion_fallback(self):
        fake = Fake(comments=[start_comment()], run_status='completed', conclusion='failure')

        def recover(call, repository, number, sha, statuses, cutoff):
            fake.comments[0]['body'] = fake.comments[0]['body'].replace('— 処理開始', '— 正常終了')
            return 1

        with patch.object(prior, 'reconcile_sha', side_effect=recover):
            self.assertEqual(subject.reconcile(fake.call, REPO, now=NOW), 1)
        self.assertIn('— 正常終了', fake.comments[0]['body'])
        self.assertNotIn('— 異常終了', fake.comments[0]['body'])
        self.assertFalse(any('/actions/runs/700/attempts/2' in call[0] for call in fake.calls))

    def test_old_or_untrusted_start_does_not_nominate_pr(self):
        comments = [
            start_comment(ident=1, created='2026-09-11T20:00:00Z'),
            start_comment(ident=2, actor='someone-else'),
        ]
        fake = Fake(comments=comments)
        with patch.object(prior, 'reconcile_sha', return_value=0) as recover:
            self.assertEqual(subject.reconcile(fake.call, REPO, now=NOW), 0)
        recover.assert_not_called()
        self.assertFalse(any(call[0] == f'repos/{REPO}/pulls/42' for call in fake.calls))

    def test_open_pr_is_not_reprocessed_by_closed_recovery(self):
        fake = Fake(comments=[start_comment()], state='open')
        with patch.object(prior, 'reconcile_sha', return_value=0) as recover:
            self.assertEqual(subject.reconcile(fake.call, REPO, now=NOW), 0)
        recover.assert_not_called()

    def test_periodic_workflow_invokes_closed_pr_recovery(self):
        workflow = (Path(__file__).resolve().parents[1] / 'workflows' / 'aadw-status-notification-reconcile.yml').read_text(encoding='utf-8')
        self.assertIn('python3 -I .github/scripts/aadw_closed_pr_reconcile.py --lookback-seconds 7200', workflow)


if __name__ == '__main__':
    unittest.main()
