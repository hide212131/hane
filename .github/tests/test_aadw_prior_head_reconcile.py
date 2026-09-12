import datetime as dt
import sys
from pathlib import Path
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import aadw_prior_head_reconcile as subject

REPO = 'hide212131/hane'
CURRENT = 'a' * 40
OLD = 'b' * 40
NOW = dt.datetime(2026, 9, 12, 3, 10, tzinfo=dt.timezone.utc)


def status(id_, state, description, target_url):
    return {
        'id': id_,
        'context': 'hane/claude-fix',
        'state': state,
        'description': description,
        'target_url': target_url,
        'created_at': '2026-09-12T03:05:00Z',
    }


class Fake:
    def __init__(self):
        marker = (
            f'<!-- hane-aadw: process=claude-fix kind=pr number=131 sha={OLD} '
            'run=777 attempt=1 -->'
        )
        self.comments = [{
            'id': 10,
            'user': {'login': 'github-actions[bot]'},
            'created_at': '2026-09-12T03:04:00Z',
            'updated_at': '2026-09-12T03:04:00Z',
            'body': '### AADW: Claude Codeによる自動修正 — 処理開始\n\n' + marker + '\n',
        }]
        self.old_statuses = [
            status(1, 'pending', 'Claude fix running for ' + OLD[:12],
                   'https://github.com/hide212131/hane/actions/runs/777'),
            status(2, 'success', 'Claude fix completed for ' + OLD[:12],
                   'https://github.com/hide212131/hane/commit/' + CURRENT),
        ]
        self.calls = []

    def call(self, endpoint, payload=None, method=None):
        self.calls.append((endpoint, payload, method))
        if '/pulls?state=open' in endpoint:
            return [{
                'number': 131, 'state': 'open', 'draft': False,
                'user': {'login': 'github-actions[bot]'},
                'head': {'sha': CURRENT, 'repo': {'full_name': REPO}},
            }]
        if f'/commits/{OLD}/statuses' in endpoint:
            return self.old_statuses
        if '/issues/131/comments?' in endpoint:
            return self.comments
        if '/issues/comments/10' in endpoint:
            self.comments[0]['body'] = payload['body']
            return self.comments[0]
        raise AssertionError(endpoint)


class PriorHeadTests(unittest.TestCase):
    def test_claude_fix_terminal_on_prior_head_updates_same_start_comment(self):
        gh = Fake()
        writes = subject.reconcile(gh.call, REPO, now=NOW)
        self.assertEqual(writes, 1)
        self.assertEqual(len(gh.comments), 1)
        body = gh.comments[0]['body']
        self.assertIn('正常終了', body)
        self.assertIn(f'sha={OLD}', body)
        self.assertIn('run=777 attempt=1', body)
        self.assertIn('Claude fix completed', body)

    def test_prior_head_reconcile_is_idempotent(self):
        gh = Fake()
        subject.reconcile(gh.call, REPO, now=NOW)
        self.assertEqual(subject.reconcile(gh.call, REPO, now=NOW), 0)
        self.assertEqual(len(gh.comments), 1)

    def test_old_start_outside_lookback_is_not_reopened(self):
        gh = Fake()
        gh.comments[0]['created_at'] = '2026-09-11T20:00:00Z'
        gh.comments[0]['updated_at'] = '2026-09-11T20:00:00Z'
        self.assertEqual(subject.reconcile(gh.call, REPO, lookback_seconds=7200, now=NOW), 0)
        self.assertIn('処理開始', gh.comments[0]['body'])


if __name__ == '__main__':
    unittest.main()
