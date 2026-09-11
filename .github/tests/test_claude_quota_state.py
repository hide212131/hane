from datetime import datetime, timezone
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "claude_quota_state.py"
SPEC = importlib.util.spec_from_file_location("claude_quota_state", SCRIPT)
policy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(policy)
SHA = "a" * 40
SHORT = SHA[:12]
NOW = datetime(2026, 9, 11, 14, 25, tzinfo=timezone.utc)


def status(id, description, state="pending"):
    return {
        "id": id,
        "context": "hane/claude-fix",
        "state": state,
        "description": description,
        "created_at": NOW.isoformat(),
    }


def wait(id, when):
    return status(id, f"Claude fix quota wait until {when} for {SHORT}")


class ParseSessionLimitTests(unittest.TestCase):
    def test_actual_session_limit_shape_gets_safe_retry_time(self):
        result = policy.parse_session_limit(
            "You've hit your session limit · resets 6:20pm (UTC)", NOW
        )
        self.assertEqual(
            result,
            {"quota_wait": True, "retry_after": "2026-09-11T18:22Z"},
        )

    def test_past_reset_time_rolls_to_next_utc_day(self):
        result = policy.parse_session_limit(
            "You've hit your session limit · resets 1:20pm (UTC)", NOW
        )
        self.assertEqual(result["retry_after"], "2026-09-12T13:22Z")

    def test_generic_failure_never_becomes_quota_wait(self):
        self.assertFalse(policy.parse_session_limit("Authentication failed", NOW)["quota_wait"])

    def test_unparseable_limit_stays_terminal(self):
        self.assertFalse(policy.parse_session_limit("You've hit your session limit", NOW)["quota_wait"])


class RecoveryTests(unittest.TestCase):
    def test_before_retry_after_does_not_recover(self):
        rows = [wait(1, "2026-09-11T18:22Z")]
        self.assertFalse(policy.recovery(rows, SHA, NOW)["recover"])

    def test_first_wait_recovers_after_retry_after(self):
        rows = [wait(1, "2026-09-11T18:22Z")]
        result = policy.recovery(
            rows, SHA, datetime(2026, 9, 11, 18, 22, tzinfo=timezone.utc)
        )
        self.assertTrue(result["recover"])
        self.assertEqual(result["retry_number"], 1)

    def test_second_wait_allows_second_retry(self):
        rows = [
            wait(1, "2026-09-11T18:22Z"),
            wait(2, "2026-09-12T18:22Z"),
        ]
        result = policy.recovery(
            rows, SHA, datetime(2026, 9, 12, 18, 23, tzinfo=timezone.utc)
        )
        self.assertTrue(result["recover"])
        self.assertEqual(result["retry_number"], 2)

    def test_third_wait_exhausts_quota_retry_budget(self):
        rows = [
            wait(1, "2026-09-11T18:22Z"),
            wait(2, "2026-09-12T18:22Z"),
            wait(3, "2026-09-13T18:22Z"),
        ]
        result = policy.recovery(
            rows, SHA, datetime(2026, 9, 13, 18, 23, tzinfo=timezone.utc)
        )
        self.assertFalse(result["recover"])
        self.assertEqual(result["quota_wait_count"], 3)

    def test_new_terminal_failure_supersedes_old_wait(self):
        rows = [
            wait(1, "2026-09-11T18:22Z"),
            status(2, f"Claude fix failed for {SHORT}", "error"),
        ]
        result = policy.recovery(
            rows, SHA, datetime(2026, 9, 12, tzinfo=timezone.utc)
        )
        self.assertFalse(result["recover"])

    def test_wrong_sha_wait_does_not_recover(self):
        rows = [
            status(
                1,
                "Claude fix quota wait until 2026-09-11T18:22Z for bbbbbbbbbbbb",
            )
        ]
        self.assertFalse(
            policy.recovery(
                rows, SHA, datetime(2026, 9, 12, tzinfo=timezone.utc)
            )["recover"]
        )


if __name__ == "__main__":
    unittest.main()
