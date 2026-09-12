import sys
import unittest
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import aadw_notify
import aadw_observer as observer

REPO = 'hide212131/hane'
SHA = 'a' * 40


def pr(body='', number=42):
    return {'number': number, 'state': 'open', 'draft': False, 'body': body,
            'base': {'repo': {'full_name': REPO}},
            'head': {'sha': SHA, 'repo': {'full_name': REPO}},
            'user': {'login': 'github-actions[bot]'}}


class Fake:
    def __init__(self):
        self.prs = {42: pr()}
        self.statuses = {SHA: []}
        self.comments = {}
        self.all_comments = []
        self.runs = {111: {'run_attempt': 2}}
        self.workflow_runs = []
        self.next_id = 1000

    def call(self, endpoint, payload=None, method=None):
        clean = endpoint.split('?', 1)[0]
        if clean == f'repos/{REPO}/commits/{SHA}/pulls': return list(self.prs.values())
        if clean == f'repos/{REPO}/pulls': return list(self.prs.values())
        if clean.startswith(f'repos/{REPO}/pulls/'): return self.prs[int(clean.rsplit('/', 1)[1])]
        if clean == f'repos/{REPO}/commits/{SHA}/statuses': return list(self.statuses[SHA])
        if clean == f'repos/{REPO}/actions/runs': return {'workflow_runs': list(self.workflow_runs)}
        if clean.startswith(f'repos/{REPO}/actions/runs/'): return self.runs[int(clean.rsplit('/', 1)[1])]
        if clean == f'repos/{REPO}/issues/comments': return list(self.all_comments)
        if clean.startswith(f'repos/{REPO}/issues/comments/'):
            cid = int(clean.rsplit('/', 1)[1])
            for rows in self.comments.values():
                for comment in rows:
                    if comment['id'] == cid:
                        comment['body'] = payload['body']; return comment
            raise RuntimeError('missing comment')
        if '/issues/' in clean and clean.endswith('/comments'):
            number = int(clean.split('/issues/')[1].split('/')[0])
            if payload is None: return list(self.comments.get(number, []))
            row = {'id': self.next_id, 'user': {'login': 'github-actions[bot]'}, 'body': payload['body'],
                   'issue_url': f'https://api.github.com/repos/{REPO}/issues/{number}'}
            self.next_id += 1; self.comments.setdefault(number, []).append(row); return row
        raise AssertionError(endpoint)

    def status(self, id, context, state, description, url='https://github.com/hide212131/hane/actions/runs/111'):
        row = {'id': id, 'sha': SHA, 'context': context, 'state': state,
               'description': description, 'target_url': url}
        self.statuses[SHA].insert(0, row)
        return {**row, 'sender': {'login': 'github-actions[bot]'}}


class Tests(unittest.TestCase):
    def test_status_start_and_terminal_share_comment(self):
        f = Fake()
        observer.handle_status(f.call, REPO, f.status(1, 'hane/codex-review', 'pending',
            'Codex review pending for aaaaaaaaaaaa'), '900', '1')
        observer.handle_status(f.call, REPO, f.status(2, 'hane/codex-review', 'failure',
            'Codex findings for aaaaaaaaaaaa', 'https://github.com/hide212131/hane/pull/42'), '901', '1')
        self.assertEqual(len(f.comments[42]), 1)
        self.assertIn('正常終了', f.comments[42][0]['body'])
        self.assertIn('/actions/runs/111/attempts/2', f.comments[42][0]['body'])

    def test_business_failure_status_can_be_normal_completion(self):
        f = Fake()
        observer.handle_status(f.call, REPO, f.status(1, 'hane/gui-requirement', 'failure',
            'GUI validation required (v1) for aaaaaaaaaaaa'), '900', '1')
        self.assertIn('正常終了', f.comments[42][0]['body'])
        self.assertIn('GUI validation必要', f.comments[42][0]['body'])

    def test_stale_routing_is_abnormal_completion(self):
        f = Fake()
        observer.handle_status(f.call, REPO, f.status(1, 'hane/copilot-routing', 'error',
            'Copilot routing stale for aaaaaaaaaaaa'), '900', '1')
        self.assertIn('異常終了', f.comments[42][0]['body'])

    def test_blocked_gui_is_normal_completion(self):
        f = Fake()
        observer.handle_status(f.call, REPO, f.status(1, 'hane/gui-validation', 'error',
            'GUI blocked v1 aaaaaaaaaaaa g111-2'), '900', '1')
        self.assertIn('正常終了', f.comments[42][0]['body'])
        self.assertIn('検証結果: blocked', f.comments[42][0]['body'])

    def test_untrusted_status_is_ignored(self):
        f = Fake(); event = f.status(1, 'hane/codex-review', 'pending', 'x')
        event['sender']['login'] = 'someone-else'
        self.assertFalse(observer.handle_status(f.call, REPO, event, '900', '1'))

    def test_workflow_timeout_closes_pending(self):
        f = Fake(); f.status(1, 'hane/claude-fix', 'pending', 'Claude fix running for aaaaaaaaaaaa')
        payload = {'action': 'completed', 'workflow_run': {'id': 111, 'run_attempt': 2,
            'name': 'Claude automatic fix worker', 'event': 'repository_dispatch', 'conclusion': 'timed_out'}}
        self.assertTrue(observer.handle_workflow_run(f.call, REPO, payload))
        self.assertIn('異常終了', f.comments[42][0]['body'])

    def test_implement_start_and_success_share_comment(self):
        f = Fake()
        observer.handle_repository_dispatch(f.call, REPO, {'action': 'claude-progress-start', 'client_payload': {
            'repository': REPO, 'issue_number': '130', 'implementation_run_id': '111', 'implementation_run_attempt': '2'}})
        progress = {'id': 50, 'user': {'login': 'claude[bot]'}, 'body': 'Hane progress run=111 attempt=2',
                    'issue_url': f'https://api.github.com/repos/{REPO}/issues/130'}
        f.comments.setdefault(130, []).append(progress); f.all_comments.append(progress)
        f.prs[42] = pr('Agentic implementation of #130.')
        observer.handle_workflow_run(f.call, REPO, {'action': 'completed', 'workflow_run': {
            'id': 111, 'run_attempt': 2, 'name': 'Implement issue with Claude', 'event': 'issue_comment', 'conclusion': 'success'}})
        lifecycle = [c for c in f.comments[130] if 'hane-aadw:' in c['body']]
        self.assertEqual(len(lifecycle), 1); self.assertIn('正常終了', lifecycle[0]['body'])

    def test_codex_limit_uses_real_fallback_run(self):
        f = Fake(); f.status(1, 'hane/codex-review', 'pending', 'Codex review pending for aaaaaaaaaaaa')
        f.workflow_runs = [{'id': 222, 'run_attempt': 1, 'name': 'Codex limit Copilot review fallback',
            'display_title': 'Example PR', 'created_at': '2026-09-12T02:00:01Z',
            'actor': {'login': 'chatgpt-codex-connector[bot]'}}]
        payload = {'action': 'created', 'issue': {'number': 42, 'title': 'Example PR', 'pull_request': {'url': 'x'}},
            'comment': {'user': {'login': 'chatgpt-codex-connector[bot]'}, 'created_at': '2026-09-12T02:00:00Z',
                        'body': 'You have reached your Codex usage limits for code reviews.'}}
        observer.handle_issue_comment(f.call, REPO, payload, '900', '1')
        bodies = [c['body'] for c in f.comments[42]]
        self.assertTrue(any('Codexレビュー — 異常終了' in b for b in bodies))
        self.assertTrue(any('/actions/runs/222/attempts/1' in b and '処理開始' in b for b in bodies))

    def test_fallback_terminal_closes_active_start(self):
        f = Fake()
        aadw_notify.notify(f.call, state='start', process='codex-review-fallback', kind='pr', number=42,
                           sha=SHA, repository=REPO, run_id='222', attempt='1')
        event = f.status(5, 'hane/review-source', 'success', 'Review source: Copilot fallback for aaaaaaaaaaaa',
                         'https://github.com/example/review')
        observer.handle_status(f.call, REPO, event, '901', '1')
        self.assertEqual(len(f.comments[42]), 1); self.assertIn('正常終了', f.comments[42][0]['body'])


if __name__ == '__main__': unittest.main()
