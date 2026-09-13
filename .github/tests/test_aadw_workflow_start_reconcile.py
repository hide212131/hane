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
        self.attempts = {
            '777': [
                '2026-09-12T02:00:00Z',
                '2026-09-12T02:10:00Z',
                '2026-09-12T02:20:00Z',
            ],
        }

    def call(self, endpoint, payload=None, method=None):
        clean = endpoint.split('?', 1)[0]
        if clean == f'repos/{REPO}/pulls':
            return [pr()]
        if clean == f'repos/{REPO}/pulls/42':
            return pr()
        if clean == f'repos/{REPO}/commits/{SHA}/statuses':
            return list(self.statuses)
        if clean == f'repos/{REPO}/actions/runs/777':
            return {'run_attempt': len(self.attempts['777'])}
        if clean.startswith(f'repos/{REPO}/actions/runs/777/attempts/'):
            attempt = int(clean.rsplit('/', 1)[1])
            return {
                'run_attempt': attempt,
                'run_started_at': self.attempts['777'][attempt - 1],
            }
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
            cid = int(clean.rsplit('/', 1)[1])
            for row in self.comments:
                if row['id'] == cid:
                    if method == 'DELETE':
                        self.comments.remove(row)
                        return None
                    row['body'] = payload['body']
                    return row
        raise AssertionError(endpoint)

    def fallback_marker(self, run='777', attempt='1'):
        self.comments.append({
            'id': self.next_id,
            'user': {'login': 'github-actions[bot]'},
            'body': (
                '### AADW: Codexレビュー利用上限時のCopilot代替レビュー — 正常終了\n\n'
                f'<!-- hane-aadw: process=codex-review-fallback kind=pr number=42 sha={SHA} '
                f'run={run} attempt={attempt} -->\n'
            ),
        })
        self.next_id += 1


class Tests(unittest.TestCase):
    def pending(self, context, run='777', description='pending', created='2026-09-12T02:11:00Z', id_=7):
        return {
            'id': id_,
            'context': context,
            'state': 'pending',
            'description': description,
            'target_url': f'https://github.com/hide212131/hane/actions/runs/{run}',
            'created_at': created,
        }

    def test_exact_workflow_attempt_creates_start(self):
        f = Fake()
        f.statuses.append(self.pending('hane/codex-review', description='Codex review pending for aaaaaaaaaaaa'))
        writes = start_reconcile.reconcile_start(
            f.call, REPO, 'Codex review gate', '777', '2', poll_attempts=1
        )
        self.assertEqual(writes, 1)
        self.assertEqual(len(f.comments), 1)
        body = f.comments[0]['body']
        self.assertIn('処理開始', body)
        self.assertIn('run=777 attempt=2', body)
        self.assertIn('/actions/runs/777/attempts/2', body)

    def test_rerun_waits_for_current_attempt_pending_instead_of_reusing_prior_attempt(self):
        f = Fake()
        old_pending = self.pending(
            'hane/codex-review',
            description='Codex review pending for aaaaaaaaaaaa',
            created='2026-09-12T02:01:00Z',
            id_=7,
        )
        new_pending = self.pending(
            'hane/codex-review',
            description='Codex review pending for aaaaaaaaaaaa',
            created='2026-09-12T02:11:00Z',
            id_=8,
        )
        status_reads = {'count': 0}

        def call(endpoint, payload=None, method=None):
            clean = endpoint.split('?', 1)[0]
            if clean == f'repos/{REPO}/commits/{SHA}/statuses':
                status_reads['count'] += 1
                return [old_pending] if status_reads['count'] == 1 else [old_pending, new_pending]
            return f.call(endpoint, payload, method)

        writes = start_reconcile.reconcile_start(
            call,
            REPO,
            'Codex review gate',
            '777',
            '2',
            poll_attempts=2,
            poll_seconds=1,
            sleeper=lambda _seconds: None,
        )
        self.assertEqual(writes, 1)
        self.assertEqual(status_reads['count'], 2)
        self.assertEqual(len(f.comments), 1)
        self.assertIn('run=777 attempt=2', f.comments[0]['body'])

    def test_rerun_with_only_previous_attempt_pending_does_not_create_start(self):
        f = Fake()
        f.statuses.append(self.pending(
            'hane/codex-review',
            description='Codex review pending for aaaaaaaaaaaa',
            created='2026-09-12T02:01:00Z',
        ))
        writes = start_reconcile.reconcile_start(
            f.call, REPO, 'Codex review gate', '777', '2', poll_attempts=1
        )
        self.assertEqual(writes, 0)
        self.assertEqual(f.comments, [])

    def test_routing_reconciliation_uses_same_routing_context_and_polls(self):
        f = Fake()
        calls = {'count': 0}

        def call(endpoint, payload=None, method=None):
            clean = endpoint.split('?', 1)[0]
            if clean == f'repos/{REPO}/commits/{SHA}/statuses':
                calls['count'] += 1
                if calls['count'] < 2:
                    return []
                return [self.pending(
                    'hane/copilot-routing',
                    description='Copilot routing pending for aaaaaaaaaaaa',
                    created='2026-09-12T02:21:00Z',
                )]
            return f.call(endpoint, payload, method)

        writes = start_reconcile.reconcile_start(
            call,
            REPO,
            'Copilot routing reconciliation',
            '777',
            '3',
            poll_attempts=2,
            poll_seconds=1,
            sleeper=lambda _seconds: None,
        )
        self.assertEqual(writes, 1)
        self.assertGreaterEqual(calls['count'], 2)
        body = f.comments[0]['body']
        self.assertIn('Copilot pre-GUI routing', body)
        self.assertIn('run=777 attempt=3', body)

    def test_fallback_rerun_creates_new_attempt_start_from_prior_marker(self):
        f = Fake()
        f.fallback_marker(run='777', attempt='1')
        writes = start_reconcile.reconcile_start(
            f.call, REPO, 'Codex limit Copilot review fallback', '777', '2', poll_attempts=1
        )
        self.assertEqual(writes, 1)
        self.assertEqual(len(f.comments), 2)
        body = f.comments[-1]['body']
        self.assertIn('処理開始', body)
        self.assertIn('process=codex-review-fallback', body)
        self.assertIn('run=777 attempt=2', body)
        self.assertIn(f'sha={SHA}', body)
        self.assertEqual(
            start_reconcile.reconcile_start(
                f.call, REPO, 'Codex limit Copilot review fallback', '777', '2', poll_attempts=1
            ),
            0,
        )

    def test_fallback_attempt_one_remains_owned_by_limit_comment_observer(self):
        f = Fake()
        writes = start_reconcile.reconcile_start(
            f.call, REPO, 'Codex limit Copilot review fallback', '777', '1', poll_attempts=1
        )
        self.assertEqual(writes, 0)
        self.assertEqual(f.comments, [])

    def test_fallback_rerun_without_prior_same_run_marker_fails_closed(self):
        f = Fake()
        f.fallback_marker(run='778', attempt='1')
        with self.assertRaises(RuntimeError):
            start_reconcile.reconcile_start(
                f.call, REPO, 'Codex limit Copilot review fallback', '777', '2', poll_attempts=1
            )

    def test_unrelated_pending_status_is_not_used(self):
        f = Fake()
        f.statuses.append(self.pending('hane/codex-review', run='778', description='Codex review pending for aaaaaaaaaaaa'))
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
