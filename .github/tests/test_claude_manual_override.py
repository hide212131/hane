"""Regression coverage for explicit owner /claude-fix routing overrides."""

import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "claude_fix_state.py"
SPEC = importlib.util.spec_from_file_location("claude_fix_state_manual_override", SCRIPT)
policy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(policy)

SHA = "a" * 40
SHORT = SHA[:12]
CONTEXT = "hane/claude-fix-manual/5609492495-1"


def status(identifier, context, state, description):
    return {
        "id": identifier,
        "context": context,
        "state": state,
        "description": description,
        "created_at": "2026-09-09T22:16:30Z",
    }


def route(identifier, kind="continue-validation"):
    return status(
        identifier,
        "hane/copilot-routing",
        "failure" if kind == "fix" else "success",
        f"Copilot routing: {kind} for {SHORT}",
    )


def grant(identifier):
    return status(
        identifier,
        CONTEXT,
        "success",
        f"Claude manual retry authorized for {SHORT}",
    )


def claim(identifier):
    return status(
        identifier,
        CONTEXT,
        "pending",
        f"Claude manual retry claimed for {SHORT}",
    )


class ManualRoutingOverrideTests(unittest.TestCase):
    def test_owner_command_supersedes_older_continue_validation(self):
        rows = [route(1), grant(2)]
        self.assertTrue(policy.manual_command_allows_fix(rows, SHA, CONTEXT))

    def test_claimed_owner_command_remains_valid_for_same_execution(self):
        rows = [route(1), grant(2), claim(4)]
        self.assertTrue(policy.manual_command_allows_fix(rows, SHA, CONTEXT))

    def test_newer_no_fix_decision_still_stops_manual_execution(self):
        rows = [route(1, "fix"), grant(2), claim(3), route(4)]
        self.assertFalse(policy.manual_command_allows_fix(rows, SHA, CONTEXT))

    def test_consumed_authorization_cannot_be_reused(self):
        rows = [
            route(1),
            grant(2),
            status(3, CONTEXT, "error", f"Claude manual retry consumed for {SHORT}"),
        ]
        self.assertFalse(policy.manual_command_allows_fix(rows, SHA, CONTEXT))

    def test_new_command_does_not_steal_preexisting_execution_lease(self):
        rows = [
            route(1),
            status(2, "hane/claude-fix", "pending", f"Claude fix running for {SHORT}"),
            grant(3),
        ]
        self.assertFalse(policy.manual_command_allows_fix(rows, SHA, CONTEXT))


if __name__ == "__main__":
    unittest.main()
