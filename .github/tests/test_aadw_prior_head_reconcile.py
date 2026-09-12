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


def status(id_, state, description, target_url, created='2026-09-12T03:05:00Z'):
    return {
        'id': id_,
        'context': 'hane/claude-fix',
        'state': state,
        'description': description,
        'target_url': target_url,
        'created_at': created,
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
        self.next_id = 11
        self.commit_shas = [OLD, CURRENT]
        self.old_statuses = [
            status(1, 'pending', 'Claude fix running for ' + OLD[:12],
                   'https://github.com/hide212131/hane/actions/runs/777'),
            status(2, 'success', 'Claude fix completed for ' + OLD[:12],
                   'https://github.com/hide212131/hane/commit/' + CURRENT),
        ]
        self.calls = []

    def call(self, endpoint, payload=None, method=None):
        self.calls.append((endpoint, payload, method))
        clean = endpoint.split('?', 1)[0]
        if clean.endswith('/pulls') and 'state=open' in endpoint:
            return [{
                'number': 131, 'state': 'open', 'draft': False,
                'user': {'login': 'github-actions[bot]'},
                'head': {'sha': CURRENT, 'repo': {'full_name': REPO}},
            }]
        if clean == f'repos/{REPO}/pulls/131/commits':
            return [{'sha': sha} for sha in self.commit_shas]
        if clean.startswith(f'repos/{REPO}/commits/') and clean.endswith('/statuses'):
            sha = clean.split('/commits/', 1)[1].split('/statuses', 1)[0]
            return self.old_statuses if sha == OLD else []
        if clean == f'repos/{REPO}/issues/131/comments':
            if payload is None:
                return self.comments
            row = {
                'id': self.next_id,
                'user': {'login': 'github-actions[bot]'},
                'created_at': '2026-09-12T03:06:00Z',
                'updated_at': '2026-09-12T03:06:00Z',
                'body': payload['body'],
            }
            self.next_id += 1
            self.comments.append(row)
            return row
        if clean.startswith(f'repos/{REPO}/issues/comments/'):
            ident = int(clean.rsplit('/', 1)[1])
            for row in list(self.comments):
                if row['id'] == ident:
                    if method == 'DELETE':
                        self.comments.remove(row)
                        return None
                    row['body'] = payload['body']
                    row['updated_at'] = '2026-09-12T03:06:00Z'
                    return row
            raise AssertionError('missing comment')
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

    def test_missed_start_is_recovered_from_prior_commit_statuses(self):
        gh = Fake()
        gh.comments = []
        writes = subject.reconcile(gh.call, REPO, now=NOW)
        self.assertEqual(writes, 1)
        self.assertEqual(len(gh.comments), 1)
        body = gh.comments[0]['body']
        self.assertIn('正常終了', body)
        self.assertIn(f'sha={OLD}', body)
        self.assertIn('run=777 attempt=1', body)

    def test_prior_head_is_not_lost_when_more_than_five_commits_follow_it(self):
        gh = Fake()
        gh.comments = []
        gh.commit_shas = [OLD, 'c' * 40, 'd' * 40, 'e' * 40, 'f' * 40, '1' * 40, CURRENT]
        writes = subject.reconcile(gh.call, REPO, now=NOW)
        self.assertEqual(writes, 1)
        self.assertEqual(len(gh.comments), 1)
        self.assertIn(f'sha={OLD}', gh.comments[0]['body'])
        scanned = [call[0] for call in gh.calls if f'/commits/{OLD}/statuses' in call[0]]
        self.assertTrue(scanned)

    def test_old_status_outside_lookback_is_not_backfilled(self):
        gh = Fake()
        gh.comments[0]['created_at'] = '2026-09-11T20:00:00Z'
        gh.comments[0]['updated_at'] = '2026-09-11T20:00:00Z'
        gh.old_statuses = [
            status(1, 'pending', 'Claude fix running for ' + OLD[:12],
                   'https://github.com/hide212131/hane/actions/runs/777', '2026-09-11T20:00:00Z'),
            status(2, 'success', 'Claude fix completed for ' + OLD[:12],
                   'https://github.com/hide212131/hane/commit/' + CURRENT, '2026-09-11T20:01:00Z'),
        ]
        self.assertEqual(subject.reconcile(gh.call, REPO, lookback_seconds=7200, now=NOW), 0)
        self.assertIn('処理開始', gh.comments[0]['body'])


if __name__ == '__main__':
    unittest.main()
