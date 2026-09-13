import unittest
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import aadw_workflow_completion_reconcile as subject

REPO = 'hide212131/hane'
OLD = 'a' * 40
CURRENT = 'b' * 40


def pr(state='open'):
    return {
        'number': 42, 'state': state, 'draft': False,
        'user': {'login': 'github-actions[bot]'},
        'base': {'repo': {'full_name': REPO}},
        'head': {'sha': CURRENT, 'repo': {'full_name': REPO}},
    }


def pending(id_=1, run=111, attempt=2, explicit_attempt=True):
    suffix = f'/attempts/{attempt}' if explicit_attempt else ''
    return {
        'id': id_, 'context': 'hane/claude-fix', 'state': 'pending',
        'description': f'Claude fix running for {OLD[:12]}',
        'target_url': f'https://github.com/hide212131/hane/actions/runs/{run}{suffix}',
        'created_at': '2026-09-12T15:00:00Z',
    }


class Fake:
    def __init__(self, old_statuses, state='open'):
        self.old_statuses = old_statuses
        self.state = state
        self.comments = []
        self.next_id = 1

    def call(self, endpoint, payload=None, method=None):
        clean = endpoint.split('?', 1)[0]
        if clean == f'repos/{REPO}/pulls':
            return [pr()] if self.state == 'open' else []
        if clean == f'repos/{REPO}/pulls/42':
            return pr(self.state)
        if clean == f'repos/{REPO}/pulls/42/commits':
            return [{'sha': OLD}, {'sha': CURRENT}]
        if clean == f'repos/{REPO}/commits/{OLD}/statuses':
            return list(self.old_statuses)
        if clean == f'repos/{REPO}/commits/{CURRENT}/statuses':
            return []
        if clean == f'repos/{REPO}/actions/runs/111':
            return {'run_attempt': 2}
        if clean == f'repos/{REPO}/actions/runs/111/attempts/1':
            return {'run_attempt': 1, 'run_started_at': '2026-09-12T14:00:00Z'}
        if clean == f'repos/{REPO}/actions/runs/111/attempts/2':
            return {'run_attempt': 2, 'run_started_at': '2026-09-12T14:59:00Z'}
        if clean == f'repos/{REPO}/issues/comments':
            return list(self.comments)
        if clean == f'repos/{REPO}/issues/42/comments':
            if payload is None:
                return list(self.comments)
            row = {'id': self.next_id, 'user': {'login': 'github-actions[bot]'}, 'body': payload['body']}
            self.next_id += 1
            self.comments.append(row)
            return row
        if clean.startswith(f'repos/{REPO}/issues/comments/'):
            ident = int(clean.rsplit('/', 1)[1])
            for row in self.comments:
                if row['id'] == ident:
                    if method == 'DELETE':
                        self.comments.remove(row)
                        return None
                    row['body'] = payload['body']
                    return row
            raise AssertionError('missing comment')
        raise AssertionError(endpoint)


def event(conclusion='timed_out', run=111, attempt=2):
    return {'action': 'completed', 'workflow_run': {
        'id': run, 'run_attempt': attempt, 'conclusion': conclusion,
        'name': 'Claude automatic fix worker',
    }}


def start_marker():
    return (
        f'<!-- hane-aadw: process=claude-fix kind=pr number=42 sha={OLD} '
        'run=111 attempt=2 -->'
    )


class Tests(unittest.TestCase):
    def test_prior_head_pending_for_completed_run_is_closed_abnormally(self):
        fake = Fake([pending()])
        self.assertEqual(subject.reconcile(fake.call, REPO, event()), 1)
        self.assertEqual(len(fake.comments), 1)
        body = fake.comments[0]['body']
        self.assertIn('異常終了', body)
        self.assertIn(f'sha={OLD}', body)
        self.assertIn('run=111 attempt=2', body)

    def test_pending_without_attempt_is_resolved_to_completed_attempt(self):
        fake = Fake([pending(explicit_attempt=False)])
        self.assertEqual(subject.reconcile(fake.call, REPO, event()), 1)
        self.assertEqual(len(fake.comments), 1)
        self.assertIn('run=111 attempt=2', fake.comments[0]['body'])
        self.assertIn('異常終了', fake.comments[0]['body'])

    def test_terminal_status_after_pending_is_not_reclosed(self):
        rows = [
            pending(),
            {
                'id': 2, 'context': 'hane/claude-fix', 'state': 'success',
                'description': f'Claude fix completed for {OLD[:12]}',
                'target_url': 'https://github.com/hide212131/hane/actions/runs/111/attempts/2',
                'created_at': '2026-09-12T15:01:00Z',
            },
        ]
        fake = Fake(rows)
        self.assertEqual(subject.reconcile(fake.call, REPO, event('success')), 0)
        self.assertEqual(fake.comments, [])

    def test_other_run_does_not_close_pending(self):
        fake = Fake([pending(run=222, attempt=1)])
        self.assertEqual(subject.reconcile(fake.call, REPO, event(run=111, attempt=2)), 0)
        self.assertEqual(fake.comments, [])

    def test_explicit_start_marker_recovers_sha_outside_commit_list(self):
        fake = Fake([pending()])
        fake.comments.append({'id': 8, 'user': {'login': 'github-actions[bot]'},
                              'body': '### AADW: Claude Codeによる自動修正 — 処理開始\n' + start_marker()})
        original = fake.call
        def call(endpoint, payload=None, method=None):
            if endpoint.split('?', 1)[0] == f'repos/{REPO}/pulls/42/commits':
                return [{'sha': CURRENT}]
            return original(endpoint, payload, method)
        self.assertEqual(subject.reconcile(call, REPO, event()), 1)
        self.assertEqual(len(fake.comments), 1)
        self.assertIn('異常終了', fake.comments[0]['body'])

    def test_closed_pr_start_marker_is_still_closed_on_workflow_completion(self):
        fake = Fake([pending()], state='closed')
        fake.comments.append({'id': 8, 'user': {'login': 'github-actions[bot]'},
                              'body': '### AADW: Claude Codeによる自動修正 — 処理開始\n' + start_marker()})
        self.assertEqual(subject.reconcile(fake.call, REPO, event()), 1)
        self.assertEqual(len(fake.comments), 1)
        self.assertIn('異常終了', fake.comments[0]['body'])
        self.assertIn('run=111 attempt=2', fake.comments[0]['body'])


if __name__ == '__main__':
    unittest.main()
