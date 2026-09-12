"""Unit tests for the shared AADW start/success/failure Issue/PR notifier."""
import json
from pathlib import Path
import subprocess
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import aadw_notify as notifier  # noqa: E402

SHA = 'a' * 40
REPO = 'hide212131/hane'


class FakeGitHub:
    """In-memory GitHub API double: comments only, keyed like the real API."""

    def __init__(self, comments=()):
        self.comments = list(comments)
        self.next_id = max([c['id'] for c in self.comments], default=0) + 1
        self.calls = []

    def call(self, endpoint, payload=None, method=None):
        self.calls.append((endpoint, payload, method))
        if endpoint.startswith(f'repos/{REPO}/issues/comments/'):
            comment_id = int(endpoint.rsplit('/', 1)[1])
            for c in self.comments:
                if c['id'] == comment_id:
                    c['body'] = payload['body']
                    return c
            raise RuntimeError('comment not found')
        if '/comments?' in endpoint:
            return list(self.comments)
        # Plain create.
        comment = {'id': self.next_id, 'user': {'login': 'github-actions[bot]'}, 'body': payload['body']}
        self.next_id += 1
        self.comments.append(comment)
        return comment


def call_notify(gh, state, **kwargs):
    return notifier.notify(gh.call, state=state, repository=REPO, run_id='111', attempt='1', **kwargs)


class NotifyTests(unittest.TestCase):
    def test_start_posts_new_comment(self):
        gh = FakeGitHub()
        result = call_notify(gh, 'start', process='codex-review', kind='pr', number=42, sha=SHA)
        self.assertEqual(result['action'], 'create')
        self.assertEqual(len(gh.comments), 1)
        self.assertIn('開始', gh.comments[0]['body'])
        self.assertIn('Codexレビュー', gh.comments[0]['body'])
        self.assertIn(SHA[:12], gh.comments[0]['body'])

    def test_success_updates_same_comment(self):
        gh = FakeGitHub()
        call_notify(gh, 'start', process='gui-validation', kind='pr', number=42, sha=SHA)
        result = call_notify(gh, 'success', process='gui-validation', kind='pr', number=42, sha=SHA, detail='結果: pass')
        self.assertEqual(result['action'], 'update')
        self.assertEqual(len(gh.comments), 1)
        self.assertIn('正常終了', gh.comments[0]['body'])
        self.assertIn('結果: pass', gh.comments[0]['body'])

    def test_failure_updates_same_comment(self):
        gh = FakeGitHub()
        call_notify(gh, 'start', process='final-judge', kind='pr', number=7, sha=SHA)
        result = call_notify(gh, 'failure', process='final-judge', kind='pr', number=7, sha=SHA, detail='timeout')
        self.assertEqual(result['action'], 'update')
        self.assertIn('異常終了', gh.comments[0]['body'])
        self.assertIn('timeout', gh.comments[0]['body'])

    def test_finish_without_prior_start_still_creates_comment(self):
        gh = FakeGitHub()
        result = call_notify(gh, 'failure', process='final-judge', kind='pr', number=7, sha=SHA)
        self.assertEqual(result['action'], 'create')
        self.assertIn('異常終了', gh.comments[0]['body'])

    def test_retry_new_attempt_does_not_overwrite_previous_run(self):
        gh = FakeGitHub()
        notifier.notify(gh.call, state='start', process='claude-fix', kind='pr', number=9, sha=SHA,
                        repository=REPO, run_id='111', attempt='1')
        notifier.notify(gh.call, state='start', process='claude-fix', kind='pr', number=9, sha=SHA,
                        repository=REPO, run_id='111', attempt='2')
        self.assertEqual(len(gh.comments), 2)

    def test_different_head_sha_does_not_overwrite_previous_run(self):
        gh = FakeGitHub()
        other_sha = 'b' * 40
        call_notify(gh, 'start', process='gui-validation', kind='pr', number=9, sha=SHA)
        call_notify(gh, 'start', process='gui-validation', kind='pr', number=9, sha=other_sha)
        self.assertEqual(len(gh.comments), 2)

    def test_reprocessing_same_event_is_idempotent(self):
        gh = FakeGitHub()
        call_notify(gh, 'start', process='implement', kind='issue', number=130, sha='none')
        call_notify(gh, 'start', process='implement', kind='issue', number=130, sha='none')
        self.assertEqual(len(gh.comments), 1)
        self.assertEqual(gh.calls[-1][2], None)  # second call was a no-op GET only, no write.

    def test_reprocessing_same_terminal_event_does_not_duplicate_patch_noise(self):
        gh = FakeGitHub()
        call_notify(gh, 'start', process='implement', kind='issue', number=130, sha='none')
        call_notify(gh, 'success', process='implement', kind='issue', number=130, sha='none', detail='done')
        writes_before = len([c for c in gh.calls if c[2] == 'PATCH'])
        call_notify(gh, 'success', process='implement', kind='issue', number=130, sha='none', detail='done')
        writes_after = len([c for c in gh.calls if c[2] == 'PATCH'])
        self.assertEqual(writes_before, writes_after)  # identical body: no redundant PATCH.
        self.assertEqual(len(gh.comments), 1)

    def test_unknown_process_is_rejected(self):
        gh = FakeGitHub()
        with self.assertRaises(ValueError):
            call_notify(gh, 'start', process='not-a-real-process', kind='pr', number=1, sha=SHA)

    def test_invalid_sha_is_rejected(self):
        gh = FakeGitHub()
        with self.assertRaises(ValueError):
            call_notify(gh, 'start', process='codex-review', kind='pr', number=1, sha='not-a-sha')

    def test_api_failure_is_not_swallowed(self):
        def failing_call(*a, **k):
            raise RuntimeError('AADW通知APIの呼び出しに失敗しました: boom')
        with self.assertRaises(RuntimeError):
            notifier.notify(failing_call, state='start', process='codex-review', kind='pr', number=1,
                            sha=SHA, repository=REPO, run_id='1', attempt='1')

    def test_foreign_comment_is_never_treated_as_existing_marker_owner(self):
        marker_text = notifier.marker('codex-review', 'pr', '42', SHA, '111', '1')
        gh = FakeGitHub([{'id': 5, 'user': {'login': 'someone-else'}, 'body': marker_text}])
        result = call_notify(gh, 'success', process='codex-review', kind='pr', number=42, sha=SHA)
        self.assertEqual(result['action'], 'create')
        self.assertEqual(len(gh.comments), 2)


class CliTests(unittest.TestCase):
    def test_cli_reports_failure_without_faking_success(self):
        script = Path(__file__).resolve().parents[1] / 'scripts' / 'aadw_notify.py'
        result = subprocess.run([sys.executable, str(script), 'start', '--process', 'codex-review',
                                '--kind', 'pr', '--number', '1'],
                                capture_output=True, text=True, env={'PATH': '/usr/bin:/bin'})
        self.assertNotEqual(result.returncode, 0)


if __name__ == '__main__':
    unittest.main()
