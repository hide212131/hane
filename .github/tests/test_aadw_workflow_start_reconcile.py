import sys
import unittest
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import aadw_workflow_start_reconcile as start_reconcile

REPO = 'hide212131/hane'
SHA = 'a' * 40


def pr():
    return {
        'number': 42,
        'state': 'open',
        'draft': False,
        'head': {'sha': SHA, 'repo': {'full_name': REPO}},
        'user': {'login': 'github-actions[bot]'},
    }


class Fake:
    def __init__(self):
        self.comments = []
        self.statuses = []
        self.next_id = 100

    def call(self, endpoint, payload=None, method=None):
        clean = endpoint.split('?', 1)[0]
        if clean == f'repos/{REPO}/pulls':
            return [pr()]
        if clean == f'repos/{REPO}/commits/{SHA}/statuses':
            return list(self.statuses)
        if clean == f'repos/{REPO}/issues/42/comments':
            if payload is None:
                return list(self.comments)
            row = {'id': self.next_id, 'user': {'login': 'github-actions[bot]'}, 'body': payload['body']}
            self.next_id += 1
            self.comments.append(row)
            return row
        if clean.startswith(f'repos/{REPO}/issues/comments/'):
            cid = int(clean.rsplit('/', 1)[1])
            for row in self.comments:
                if row['id'] == cid:
                    if method == 'DELETE':
                        self.comments.remove(row)
                        return None
                    row['body'] = payload['body']
                    return row
        raise AssertionError(endpoint)


class Tests(unittest.TestCase):
    def test_exact_workflow_attempt_creates_start(self):
        f = Fake()
        f.statuses.append({
            'id': 7,
            'context': 'hane/codex-review',
            'state': 'pending',
            'description': 'Codex review pending for aaaaaaaaaaaa',
            'target_url': 'https://github.com/hide212131/hane/actions/runs/777',
        })
        writes = start_reconcile.reconcile_start(
            f.call, REPO, 'Codex review gate', '777', '2', poll_attempts=1
        )
        self.assertEqual(writes, 1)
        self.assertEqual(len(f.comments), 1)
        body = f.comments[0]['body']
        self.assertIn('処理開始', body)
        self.assertIn('run=777 attempt=2', body)
        self.assertIn('/actions/runs/777/attempts/2', body)

    def test_unrelated_pending_status_is_not_used(self):
        f = Fake()
        f.statuses.append({
            'id': 7,
            'context': 'hane/codex-review',
            'state': 'pending',
            'description': 'Codex review pending for aaaaaaaaaaaa',
            'target_url': 'https://github.com/hide212131/hane/actions/runs/778',
        })
        writes = start_reconcile.reconcile_start(
            f.call, REPO, 'Codex review gate', '777', '2', poll_attempts=1
        )
        self.assertEqual(writes, 0)
        self.assertEqual(f.comments, [])

    def test_unknown_workflow_is_ignored(self):
        f = Fake()
        writes = start_reconcile.reconcile_start(
            f.call, REPO, 'Unrelated workflow', '777', '1', poll_attempts=1
        )
        self.assertEqual(writes, 0)


if __name__ == '__main__':
    unittest.main()
