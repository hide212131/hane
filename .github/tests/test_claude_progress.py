"""Offline tests for the read-only public progress observer."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

PATH = Path(__file__).resolve().parents[1] / "scripts" / "watch_claude_progress.py"
spec = importlib.util.spec_from_file_location("progress", PATH)
progress = importlib.util.module_from_spec(spec)
spec.loader.exec_module(progress)
REPO = "hide212131/hane"
START = "2026-09-07T13:11:01Z"


def job(status="in_progress", conclusion=None):
    return {"name": "implement", "run_id": 123, "started_at": START,
            "completed_at": "2026-09-07T13:30:00Z" if status == "completed" else None,
            "status": status, "conclusion": conclusion}


def comment(body="- [ ] 設計を確認します。", cid=55):
    return {"id": cid, "body": body, "user": {"id": progress.BOT_ID, "login": "claude[bot]", "type": "Bot"},
            "performed_via_github_app": {"id": progress.APP_ID, "slug": "claude"},
            "issue_url": f"https://api.github.com/repos/{REPO}/issues/77",
            "created_at": "2026-09-07T13:11:09Z"}


class Clock:
    value = 0
    def now(self):
        return self.value
    def sleep(self, seconds):
        self.value += seconds


class FakeAPI:
    def __init__(self, frames):
        self.frames = frames
        self.index = -1
        self.calls = []
    def job(self, run_id, attempt):
        self.calls.append((run_id, attempt))
        self.index = min(self.index + 1, len(self.frames) - 1)
        frame = self.frames[self.index]
        if isinstance(frame, Exception):
            raise frame
        return frame[0]
    def comments(self, issue, since):
        return self.frames[self.index][1]


class ProgressTests(unittest.TestCase):
    def choose(self, comments, bound=None, current=None):
        return progress.choose_comment(comments, REPO, 77, 123, current or job(), bound)

    def observe(self, frames, timeout=180):
        clock = Clock()
        api = FakeAPI(frames)
        output = []
        result = progress.watch(api, REPO, 77, 123, 2,
                                lambda line: output.append((clock.now(), line)),
                                clock=clock.now, sleep=clock.sleep, timeout=timeout)
        return result, output, api

    def test_official_current_comment_selected(self):
        self.assertEqual(self.choose([comment()])["id"], 55)

    def test_older_attempt_excluded_even_if_updated_recently(self):
        old = comment()
        old["created_at"] = "2026-09-07T12:00:00Z"
        old["updated_at"] = "2026-09-07T13:15:00Z"
        self.assertIsNone(self.choose([old]))

    def test_other_authors_apps_issues_and_bad_shapes_rejected(self):
        variants = [None, {}, comment()]
        variants[-1]["user"] = "claude[bot]"
        for key, val in [("user", {"id": 5, "login": "claude[bot]", "type": "Bot"}),
                         ("performed_via_github_app", {"id": 9, "slug": "claude"}),
                         ("issue_url", f"https://api.github.com/repos/{REPO}/issues/78"),
                         ("created_at", "bad"), ("body", []), ("id", True)]:
            c = comment(); c[key] = val; variants.append(c)
        for c in variants:
            with self.subTest(c=c):
                self.assertIsNone(self.choose([c]))

    def test_ambiguous_comments_not_guessed(self):
        self.assertIsNone(self.choose([comment(cid=1), comment(cid=2)]))

    def test_exact_run_link_preferred(self):
        c = comment("[View job](https://github.com/hide212131/hane/actions/runs/123)", 2)
        self.assertEqual(self.choose([comment(cid=1), c])["id"], 2)

    def test_other_attempt_marker_rejected(self):
        self.assertIsNone(self.choose([comment("Hane progress run=123 attempt=2")]))
        self.assertEqual(self.choose([comment("Hane progress run=123 attempt=1")])["id"], 55)

    def test_other_run_link_excluded(self):
        self.assertIsNone(self.choose([comment("https://github.com/hide212131/hane/actions/runs/1234")]))

    def test_bound_id_survives_link_removal_and_never_rebinds(self):
        self.assertEqual(self.choose([comment(cid=55), comment(cid=56)], bound=55)["id"], 55)
        self.assertIsNone(self.choose([comment(cid=56)], bound=55))

    def test_after_job_completion_excluded(self):
        c = comment(); c["created_at"] = "2026-09-07T14:00:00Z"
        self.assertIsNone(self.choose([c], current=job("completed", "success")))

    def test_no_job_start_does_not_select(self):
        j = job(); j["started_at"] = None
        self.assertIsNone(self.choose([comment()], current=j))

    def test_removes_code_html_hidden_content_and_correlation_line(self):
        body = """### Plan
<!-- hidden
secret -->
<img src="anything"> spinner
Hane progress run=123 attempt=2
- [x] 設計を確認しました。
```text
::error::SECRET
```
~~~sh
SECRET2
~~~
    indented secret
次にテストを実行します。
[View job](https://github.com/anything)
"""
        self.assertEqual(progress.public_lines(body), ["- [x] 設計を確認しました。", "次にテストを実行します。"])

    def test_disarms_workflow_and_terminal_commands_after_entity_decoding(self):
        line = progress.safe_line("\x1b[31m&#58;&#58;error&#58;&#58;x ##[error] y\r\x00\u202e")
        self.assertNotIn("::", line)
        self.assertNotIn("##[", line)
        self.assertNotIn("\x1b", line)
        self.assertNotIn("\r", line)
        self.assertNotIn("\u202e", line)

    def test_known_credential_formats_removed(self):
        for token in ("github_pat_FAKEsecret_123", "ghp_FAKEsecret", "sk-ant-123456789012345"):
            self.assertNotIn(token, progress.safe_line("saved " + token))
        self.assertNotIn("aSECRET", progress.safe_line("Authorization: Bearer aSECRET"))

    def test_bounded_output(self):
        self.assertEqual(len(progress.safe_line("あ" * 1000)), 400)
        self.assertEqual(len(progress.public_lines("line\n" * 300)), 100)

    def test_live_deltas_print_before_completion_without_duplicates(self):
        first = comment("- [ ] 設計を確認します。")
        second = comment("- [x] 設計を確認しました。\n次に実装します。")
        frames = [(job(), [first]), (job(), [first]), (job(), [second]),
                  (job("completed", "success"), [second])]
        result, output, api = self.observe(frames)
        self.assertEqual(result, 0)
        self.assertEqual(sum("設計を確認しました。" in line for _, line in output), 1)
        self.assertTrue(any(t < 45 and "設計を確認しました。" in line for t, line in output))
        self.assertTrue(all(pair == (123, 2) for pair in api.calls))

    def test_reopened_checklist_is_reported(self):
        open_ = comment("- [ ] 保存")
        done = comment("- [x] 保存")
        _, output, _ = self.observe([(job(), [open_]), (job(), [done]), (job("completed", "failure"), [open_])])
        self.assertEqual(sum("- [ ] 保存" in line for _, line in output), 2)

    def test_heartbeat_does_not_claim_agent_progress(self):
        _, output, _ = self.observe([(job(), [])], timeout=80)
        self.assertTrue(any(t == 60 and "まだ進捗報告" in line and "保証" in line for t, line in output))
        self.assertFalse(any("[Claude の報告]" in line for _, line in output))

    def test_failure_cancel_skip_unknown_end_as_informational(self):
        for value in ("failure", "cancelled", "skipped", "timed_out", ["untrusted"]):
            with self.subTest(value=value):
                result, output, _ = self.observe([(job("completed", value), [])])
                self.assertEqual(result, 0)
                self.assertIn(value if isinstance(value, str) else "unknown", output[-1][1])

    def test_final_poll_reads_final_comment(self):
        _, output, _ = self.observe([(job("completed", "failure"), [comment("検証は未完了です。")])])
        self.assertIn("検証は未完了です。", output[2][1])

    def test_api_outage_bounded_and_no_exception_leak(self):
        result, output, api = self.observe([progress.Unavailable("SECRET")])
        self.assertEqual(result, 2)
        self.assertEqual(len(api.calls), 5)
        self.assertNotIn("SECRET", str(output))

    def test_transient_api_failure_recovers(self):
        result, output, _ = self.observe([progress.Unavailable(), (job("completed", "success"), [comment()])])
        self.assertEqual(result, 0)
        self.assertTrue(any("[Claude の報告]" in line for _, line in output))

    def test_missing_job_expires_without_claiming_success(self):
        result, output, _ = self.observe([(None, [])], timeout=30)
        self.assertEqual(result, 2)
        self.assertIn("未判定", output[-1][1])

    def test_pages_and_no_silent_truncation(self):
        api = progress.GitHubAPI(REPO, "fake")
        with patch.object(api, "get", side_effect=[([1], True), ([2], False)]):
            self.assertEqual(api.pages("/issues/77/comments?since=x"), [1, 2])
        with patch.object(api, "get", return_value=([1], True)) as get:
            with self.assertRaises(progress.Unavailable):
                api.pages("/issues/77/comments")
            self.assertEqual(get.call_count, 5)

    def test_wrong_response_shape_rejected(self):
        api = progress.GitHubAPI(REPO, "fake")
        with patch.object(api, "get", return_value=({"message": "SECRET"}, False)):
            with self.assertRaises(progress.Unavailable):
                api.pages("/issues/77/comments")

    def test_no_redirect_and_invalid_repo(self):
        self.assertIsNone(progress.NoRedirect().redirect_request(None, None, 302, "", {}, "https://attacker"))
        for repo in ("../bad", "a/b/c", "a/b?secret", "a/.."):
            with self.assertRaises(progress.Unavailable):
                progress.GitHubAPI(repo, "fake")


if __name__ == "__main__":
    unittest.main()
