import sys
import unittest
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import aadw_implement_completion_reconcile as reconcile
import aadw_notify

REPO = 'hide212131/hane'
SHA = 'a' * 40


def pr(body='Agentic implementation of #130.'):
    return {
        'number': 42,
        'state': 'open',
        'draft': False,
        'body': body,
        'base': {'repo': {'full_name': REPO}},
        'head': {'sha': SHA, 'repo': {'full_name': REPO}},
        'user': {'login': 'github-actions[bot]'},
    }


class Fake:
    def __init__(self):
        self.issue_comments = {}
        self.all_comments = []
        self.prs = []
        self.next_id = 100

    def call(self, endpoint, payload=None, method=None):
        clean = endpoint.split('?', 1)[0]
        if clean == f'repos/{REPO}/issues/comments':
            return list(self.all_comments)
        if clean == f'repos/{REPO}/pulls':
            return list(self.prs)
        if clean.startswith(f'repos/{REPO}/issues/comments/'):
            cid = int(clean.rsplit('/', 1)[1])
            for rows in self.issue_comments.values():
                for row in rows:
                    if row['id'] == cid:
                        if method == 'DELETE':
                            rows.remove(row)
                            if row in self.all_comments:
                                self.all_comments.remove(row)
                            return None
                        row['body'] = payload['body']
                        return row
            raise AssertionError(endpoint)
        if '/issues/' in clean and clean.endswith('/comments'):
            number = int(clean.split('/issues/')[1].split('/')[0])
            if payload is None:
                return list(self.issue_comments.get(number, []))
            row = {
                'id': self.next_id,
                'user': {'login': 'github-actions[bot]'},
                'body': payload['body'],
                'issue_url': f'https://api.github.com/repos/{REPO}/issues/{number}',
            }
            self.next_id += 1
            self.issue_comments.setdefault(number, []).append(row)
            self.all_comments.append(row)
            return row
        raise AssertionError(endpoint)


class Tests(unittest.TestCase):
    def start(self, fake):
        aadw_notify.notify(
            fake.call,
            state='start',
            process='implement',
            kind='issue',
            number=130,
            sha='none',
            repository=REPO,
            run_id='777',
            attempt='2',
            detail='Claude Codeによる実装を開始しました。',
        )

    def test_failure_without_claude_progress_closes_start_marker(self):
        f = Fake()
        self.start(f)
        action = reconcile.reconcile(f.call, REPO, '777', '2', 'failure')
        self.assertEqual(action, 'update')
        self.assertEqual(len(f.issue_comments[130]), 1)
        body = f.issue_comments[130][0]['body']
        self.assertIn('異常終了', body)
        self.assertIn('run=777 attempt=2', body)
        self.assertIn('failure', body)

    def test_success_without_claude_progress_uses_implementation_pr(self):
        f = Fake()
        self.start(f)
        f.prs = [pr()]
        action = reconcile.reconcile(f.call, REPO, '777', '2', 'success')
        self.assertEqual(action, 'update')
        self.assertEqual(len(f.issue_comments[130]), 1)
        body = f.issue_comments[130][0]['body']
        self.assertIn('正常終了', body)
        self.assertIn('実装PR #42', body)

    def test_pre_start_failure_uses_exact_run_title_issue(self):
        f = Fake()
        # Even with unrelated /implement comments present, run-name is authoritative.
        f.all_comments.extend([
            {
                'id': 10,
                'user': {'login': 'hide212131'},
                'body': '/implement',
                'created_at': '2026-09-12T03:00:00Z',
                'issue_url': f'https://api.github.com/repos/{REPO}/issues/131',
            },
            {
                'id': 11,
                'user': {'login': 'hide212131'},
                'body': '/implement',
                'created_at': '2026-09-12T03:00:00Z',
                'issue_url': f'https://api.github.com/repos/{REPO}/issues/132',
            },
        ])
        action = reconcile.reconcile(
            f.call, REPO, '777', '2', 'failure',
            display_title='Implement issue #130',
        )
        self.assertEqual(action, 'create')
        lifecycle = [c for c in f.issue_comments[130] if 'hane-aadw:' in c['body']]
        self.assertEqual(len(lifecycle), 1)
        self.assertIn('異常終了', lifecycle[0]['body'])
        self.assertIn('run=777 attempt=2', lifecycle[0]['body'])
        self.assertNotIn(131, f.issue_comments)
        self.assertNotIn(132, f.issue_comments)

    def test_run_title_must_match_exact_format(self):
        f = Fake()
        for value in ('', 'Implement issue 130', 'Implement issue #130 extra', 'Issue #130'):
            with self.subTest(value=value):
                with self.assertRaises(RuntimeError):
                    reconcile.reconcile(
                        f.call, REPO, '777', '2', 'failure',
                        display_title=value,
                    )


if __name__ == '__main__':
    unittest.main()
