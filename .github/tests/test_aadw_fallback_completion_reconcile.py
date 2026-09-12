import unittest
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import aadw_fallback_completion_reconcile as subject
import aadw_notify

REPO = 'hide212131/hane'
SHA = 'a' * 40


def pr():
    return {
        'number': 42, 'state': 'open', 'draft': False,
        'user': {'login': 'github-actions[bot]'},
        'base': {'repo': {'full_name': REPO}},
        'head': {'sha': SHA, 'repo': {'full_name': REPO}},
    }


class Fake:
    def __init__(self, statuses=()):
        self.statuses = list(statuses)
        self.comments = []
        self.next_id = 1

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
            ident = int(clean.rsplit('/', 1)[1])
            for row in list(self.comments):
                if row['id'] == ident:
                    if method == 'DELETE':
                        self.comments.remove(row)
                        return None
                    row['body'] = payload['body']
                    return row
            raise AssertionError('missing comment')
        raise AssertionError(endpoint)

    def start(self):
        aadw_notify.notify(
            self.call, state='start', process='codex-review-fallback', kind='pr', number=42,
            sha=SHA, repository=REPO, run_id='222', attempt='1',
        )


def event(conclusion='success'):
    return {'action': 'completed', 'workflow_run': {
        'id': 222, 'run_attempt': 1,
        'name': 'Codex limit Copilot review fallback',
        'conclusion': conclusion,
    }}


class Tests(unittest.TestCase):
    def test_success_exit_without_review_source_closes_active_start_as_abnormal(self):
        fake = Fake()
        fake.start()
        self.assertEqual(subject.reconcile(fake.call, REPO, event('success')), 1)
        self.assertEqual(len(fake.comments), 1)
        self.assertIn('異常終了', fake.comments[0]['body'])
        self.assertIn('stale', fake.comments[0]['body'])

    def test_review_source_success_closes_active_start_normally(self):
        fake = Fake([{
            'id': 7, 'context': 'hane/review-source', 'state': 'success',
            'description': f'Review source: Copilot fallback for {SHA[:12]}',
            'target_url': 'https://github.com/example/review',
        }])
        fake.start()
        self.assertEqual(subject.reconcile(fake.call, REPO, event('success')), 1)
        self.assertIn('正常終了', fake.comments[0]['body'])

    def test_non_success_exit_without_terminal_evidence_is_abnormal(self):
        fake = Fake()
        fake.start()
        subject.reconcile(fake.call, REPO, event('cancelled'))
        self.assertIn('異常終了', fake.comments[0]['body'])
        self.assertIn('cancelled', fake.comments[0]['body'])

    def test_already_closed_comment_is_noop(self):
        fake = Fake()
        fake.start()
        aadw_notify.notify(
            fake.call, state='failure', process='codex-review-fallback', kind='pr', number=42,
            sha=SHA, repository=REPO, run_id='222', attempt='1', detail='already closed',
        )
        body = fake.comments[0]['body']
        self.assertEqual(subject.reconcile(fake.call, REPO, event('success')), 0)
        self.assertEqual(fake.comments[0]['body'], body)

    def test_other_workflow_is_ignored(self):
        fake = Fake()
        fake.start()
        payload = event()
        payload['workflow_run']['name'] = 'Codex review gate'
        self.assertEqual(subject.reconcile(fake.call, REPO, payload), 0)
        self.assertIn('処理開始', fake.comments[0]['body'])


if __name__ == '__main__':
    unittest.main()
