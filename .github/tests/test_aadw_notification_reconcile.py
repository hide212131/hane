import datetime as dt
import re
import sys
from pathlib import Path
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
import aadw_notification_reconcile as subject

SHA = "a" * 40
REPO = "hide212131/hane"
NOW = dt.datetime(2026, 9, 12, 3, 0, tzinfo=dt.timezone.utc)


def status(id_, context, state, description, run=100, created="2026-09-12T02:55:00Z", target_url=None):
    return {
        "id": id_, "context": context, "state": state, "description": description,
        "target_url": target_url or f"https://github.com/hide212131/hane/actions/runs/{run}",
        "created_at": created,
    }


class Fake:
    def __init__(self, statuses, comments=None, attempts=None):
        self.statuses = statuses
        self.comments = list(comments or [])
        self.calls = []
        self.next_id = max([c["id"] for c in self.comments], default=0) + 1
        self.attempts = attempts or {100: ["2026-09-12T02:50:00Z"]}

    def call(self, endpoint, payload=None, method=None):
        self.calls.append((endpoint, payload, method))
        clean = endpoint.split('?', 1)[0]
        if "/pulls?state=open" in endpoint:
            return [{
                "number": 131, "state": "open", "draft": False,
                "user": {"login": "github-actions[bot]"},
                "head": {"sha": SHA, "repo": {"full_name": REPO}},
            }]
        if f"/commits/{SHA}/statuses" in endpoint:
            return self.statuses
        match = re.fullmatch(rf"repos/{re.escape(REPO)}/actions/runs/([1-9][0-9]*)(?:/attempts/([1-9][0-9]*))?", clean)
        if match:
            run_id = int(match.group(1))
            starts = self.attempts.get(run_id)
            if not starts:
                raise AssertionError(f"missing Actions run {run_id}")
            if match.group(2) is None:
                return {"run_attempt": len(starts)}
            attempt = int(match.group(2))
            return {"run_attempt": attempt, "run_started_at": starts[attempt - 1]}
        if "/comments?" in endpoint:
            return self.comments
        if "/issues/comments/" in endpoint:
            ident = int(endpoint.rsplit("/", 1)[1])
            for row in list(self.comments):
                if row["id"] == ident:
                    if method == "DELETE":
                        self.comments.remove(row)
                        return None
                    row["body"] = payload["body"]
                    return row
            raise AssertionError("missing comment")
        if endpoint.endswith("/comments"):
            row = {"id": self.next_id, "user": {"login": "github-actions[bot]"}, "body": payload["body"]}
            self.next_id += 1
            self.comments.append(row)
            return row
        raise AssertionError(endpoint)


class ReconcileTests(unittest.TestCase):
    def test_pending_then_clean_updates_one_comment(self):
        gh = Fake([
            status(10, "hane/codex-review", "pending", "Codex review pending for " + SHA[:12]),
            status(11, "hane/codex-review", "success", "Codex review clean for " + SHA[:12]),
        ])
        self.assertEqual(subject.reconcile(gh.call, REPO, now=NOW), 1)
        self.assertEqual(len(gh.comments), 1)
        self.assertIn("正常終了", gh.comments[0]["body"])
        self.assertIn("run=100 attempt=1", gh.comments[0]["body"])
        self.assertEqual(subject.reconcile(gh.call, REPO, now=NOW), 0)

    def test_terminal_same_run_without_attempt_stays_in_open_generation(self):
        rows = [
            status(10, "hane/codex-review", "pending", "Codex review pending for " + SHA[:12], run=77),
            status(11, "hane/codex-review", "success", "Codex review clean for " + SHA[:12], run=77),
        ]
        generations = subject.build_generations(rows)
        self.assertEqual(len(generations), 1)
        self.assertEqual((generations[0]["run_id"], generations[0]["attempt"]), ("77", None))
        self.assertEqual(generations[0]["latest"]["id"], 11)

    def test_terminal_generation_does_not_absorb_later_unattributed_status(self):
        rows = [
            status(10, "hane/codex-review", "pending", "Codex review pending for " + SHA[:12], run=77),
            status(11, "hane/codex-review", "error", "Codex review controller failed for " + SHA[:12], run=77),
            status(12, "hane/codex-review", "success", "Codex review clean for " + SHA[:12],
                   target_url="https://github.com/hide212131/hane/pull/131"),
        ]
        generations = subject.build_generations(rows)
        self.assertEqual(len(generations), 2)
        self.assertEqual(generations[0]["latest"]["id"], 11)
        self.assertEqual(generations[1]["latest"]["id"], 12)

    def test_new_pending_same_run_starts_new_unknown_attempt_generation(self):
        rows = [
            status(10, "hane/gui-requirement", "pending", "GUI requirement classification pending for " + SHA[:12], run=77),
            status(11, "hane/gui-requirement", "success", "GUI validation not required (v1) for " + SHA[:12], run=77),
            status(12, "hane/gui-requirement", "pending", "GUI requirement classification pending for " + SHA[:12], run=77),
        ]
        generations = subject.build_generations(rows)
        self.assertEqual([(g["run_id"], g["attempt"]) for g in generations], [("77", None), ("77", None)])

    def test_first_observed_status_can_belong_to_rerun_attempt_two(self):
        gh = Fake([
            status(12, "hane/codex-review", "pending", "Codex review pending for " + SHA[:12], run=77,
                   created="2026-09-12T02:56:00Z"),
            status(13, "hane/codex-review", "success", "Codex review clean for " + SHA[:12], run=77,
                   created="2026-09-12T02:57:00Z"),
        ], attempts={77: ["2026-09-12T02:40:00Z", "2026-09-12T02:55:00Z"]})
        subject.reconcile(gh.call, REPO, now=NOW)
        self.assertEqual(len(gh.comments), 1)
        self.assertIn("run=77 attempt=2", gh.comments[0]["body"])
        self.assertIn("/actions/runs/77/attempts/2", gh.comments[0]["body"])

    def test_gui_required_failure_status_is_normal_completion(self):
        gh = Fake([
            status(20, "hane/gui-requirement", "pending", "GUI requirement classification pending for " + SHA[:12]),
            status(21, "hane/gui-requirement", "failure", "GUI validation required (v1) for " + SHA[:12]),
        ])
        subject.reconcile(gh.call, REPO, now=NOW)
        self.assertIn("正常終了", gh.comments[0]["body"])

    def test_controller_error_is_abnormal(self):
        gh = Fake([
            status(30, "hane/gui-requirement", "pending", "GUI requirement classification pending for " + SHA[:12]),
            status(31, "hane/gui-requirement", "error", "GUI requirement classification controller failed for " + SHA[:12] + " (v1)"),
        ])
        subject.reconcile(gh.call, REPO, now=NOW)
        self.assertIn("異常終了", gh.comments[0]["body"])

    def test_routing_blocked_is_normal_routing_completion(self):
        gh = Fake([
            status(40, "hane/copilot-routing", "pending", "Copilot routing pending for " + SHA[:12]),
            status(41, "hane/copilot-routing", "error", "Copilot routing: blocked for " + SHA[:12]),
        ])
        subject.reconcile(gh.call, REPO, now=NOW)
        self.assertIn("正常終了", gh.comments[0]["body"])

    def test_gui_generation_uses_explicit_run_attempt_and_fail_is_normal(self):
        gh = Fake([
            status(42, "hane/gui-validation", "pending", f"GUI pending v1 {SHA[:12]} g888-3", run=888),
            status(43, "hane/gui-validation", "failure", f"GUI fail v1 {SHA[:12]} g888-3", run=888),
        ])
        subject.reconcile(gh.call, REPO, now=NOW)
        self.assertEqual(len(gh.comments), 1)
        self.assertIn("正常終了", gh.comments[0]["body"])
        self.assertIn("run=888 attempt=3", gh.comments[0]["body"])

    def test_gui_blocked_is_normal_completion(self):
        gh = Fake([
            status(44, "hane/gui-validation", "pending", f"GUI pending v1 {SHA[:12]} g889-1", run=889),
            status(45, "hane/gui-validation", "error", f"GUI blocked v1 {SHA[:12]} g889-1", run=889),
        ])
        subject.reconcile(gh.call, REPO, now=NOW)
        self.assertIn("正常終了", gh.comments[0]["body"])

    def test_final_blocked_is_normal_judge_completion_and_preserves_attempt(self):
        target = "https://github.com/hide212131/hane/actions/runs/990/attempts/2"
        gh = Fake([
            status(46, "hane/final-judge", "pending", f"Final pending v1 {SHA[:12]} e1234abcd", target_url=target),
            status(47, "hane/final-judge", "error", f"Final blocked v1 {SHA[:12]} e1234abcd", target_url=target),
        ])
        subject.reconcile(gh.call, REPO, now=NOW)
        self.assertIn("正常終了", gh.comments[0]["body"])
        self.assertIn("run=990 attempt=2", gh.comments[0]["body"])

    def test_fallback_compat_status_does_not_rewrite_original_codex_lifecycle(self):
        marker = f"<!-- hane-aadw: process=codex-review-fallback kind=pr number=131 sha={SHA} run=555 attempt=1 -->"
        comments = [{
            "id": 9,
            "user": {"login": "github-actions[bot]"},
            "body": "### AADW: Copilot代替レビュー — 処理開始\n\n" + marker + "\n",
        }]
        gh = Fake([
            status(50, "hane/codex-review", "pending", "Codex review pending for " + SHA[:12], run=100),
            status(51, "hane/codex-review", "error", "Codex review controller failed for " + SHA[:12], run=100,
                   created="2026-09-12T02:56:00Z"),
            status(52, "hane/review-source", "success", "Review source: Copilot fallback for " + SHA[:12], run=999,
                   created="2026-09-12T02:57:00Z"),
            status(53, "hane/codex-review", "success", "Codex review clean for " + SHA[:12],
                   created="2026-09-12T02:58:00Z", target_url="https://github.com/hide212131/hane/pull/131"),
        ], comments=comments)
        subject.reconcile(gh.call, REPO, now=NOW)
        self.assertEqual(len(gh.comments), 2)
        codex = next(c["body"] for c in gh.comments if "### AADW: Codexレビュー" in c["body"])
        fallback = next(c["body"] for c in gh.comments if "### AADW: Copilot代替レビュー" in c["body"])
        self.assertIn("異常終了", codex)
        self.assertNotIn("正常終了", codex)
        self.assertIn("正常終了", fallback)

    def test_fallback_terminal_updates_observer_start_comment(self):
        marker = f"<!-- hane-aadw: process=codex-review-fallback kind=pr number=131 sha={SHA} run=555 attempt=1 -->"
        comments = [{
            "id": 9,
            "user": {"login": "github-actions[bot]"},
            "body": "### AADW: Copilot代替レビュー — 処理開始\n\n" + marker + "\n",
        }]
        gh = Fake([
            status(61, "hane/review-source", "success", "Review source: Copilot fallback for " + SHA[:12], run=999),
        ], comments=comments)
        subject.reconcile(gh.call, REPO, now=NOW)
        self.assertEqual(len(gh.comments), 1)
        self.assertIn("正常終了", gh.comments[0]["body"])
        self.assertIn("run=555 attempt=1", gh.comments[0]["body"])

    def test_old_statuses_are_not_backfilled(self):
        gh = Fake([
            status(70, "hane/codex-review", "pending", "Codex review pending for " + SHA[:12],
                   created="2026-09-11T20:00:00Z"),
            status(71, "hane/codex-review", "success", "Codex review clean for " + SHA[:12],
                   created="2026-09-11T20:01:00Z"),
        ])
        self.assertEqual(subject.reconcile(gh.call, REPO, lookback_seconds=7200, now=NOW), 0)
        self.assertEqual(gh.comments, [])


if __name__ == "__main__":
    unittest.main()
