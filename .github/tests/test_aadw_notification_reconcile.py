import datetime as dt
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
    def __init__(self, statuses, comments=None):
        self.statuses = statuses
        self.comments = list(comments or [])
        self.calls = []
        self.next_id = max([c["id"] for c in self.comments], default=0) + 1

    def call(self, endpoint, payload=None, method=None):
        self.calls.append((endpoint, payload, method))
        if "/pulls?state=open" in endpoint:
            return [{
                "number": 131, "state": "open", "draft": False,
                "user": {"login": "github-actions[bot]"},
                "head": {"sha": SHA, "repo": {"full_name": REPO}},
            }]
        if f"/commits/{SHA}/statuses" in endpoint:
            return self.statuses
        if "/comments?" in endpoint:
            return self.comments
        if "/issues/comments/" in endpoint:
            ident = int(endpoint.rsplit("/", 1)[1])
            for row in self.comments:
                if row["id"] == ident:
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
        self.assertEqual((generations[0]["run_id"], generations[0]["attempt"]), ("77", "1"))
        self.assertEqual(generations[0]["latest"]["id"], 11)

    def test_new_pending_same_run_becomes_attempt_two_generation(self):
        rows = [
            status(10, "hane/gui-requirement", "pending", "GUI requirement classification pending for " + SHA[:12], run=77),
            status(11, "hane/gui-requirement", "success", "GUI validation not required (v1) for " + SHA[:12], run=77),
            status(12, "hane/gui-requirement", "pending", "GUI requirement classification pending for " + SHA[:12], run=77),
        ]
        generations = subject.build_generations(rows)
        self.assertEqual([(g["run_id"], g["attempt"]) for g in generations], [("77", "1"), ("77", "2")])

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

    def test_codex_fallback_provenance_is_shown(self):
        gh = Fake([
            status(50, "hane/codex-review", "pending", "Codex review pending for " + SHA[:12]),
            status(51, "hane/review-source", "success", "Review source: Copilot fallback for " + SHA[:12]),
            status(52, "hane/codex-review", "success", "Codex review clean for " + SHA[:12]),
        ])
        subject.reconcile(gh.call, REPO, now=NOW)
        self.assertIn("GitHub Copilot fallback", gh.comments[0]["body"])

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
