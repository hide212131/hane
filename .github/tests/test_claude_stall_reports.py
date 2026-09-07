"""Exercise the embedded stall-report diagnostics in implement.yml.

These blocks decide whether/what to post to the Issue or Pull Request when
Claude Code did not finish normally. They stay embedded in the trusted
workflow (not a separate .github/scripts file) for the same reason as the
existing failure summary: after the Claude step runs, the checked-out
working tree may contain agent-authored content, so any script read from
disk at that point could have been tampered with. Embedding the code as a
literal heredoc in the YAML keeps it immune to that.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

WORKFLOW = (Path(__file__).resolve().parents[1] / "workflows" / "implement.yml").read_text()


def extract(begin, end):
    return textwrap.dedent(WORKFLOW.split(begin + "\n", 1)[1].split("          " + end, 1)[0])


STALL_CODE = extract("# BEGIN CLAUDE STALL REPORT (implement)", "# END CLAUDE STALL REPORT (implement)")
DENIAL_CODE = extract("# BEGIN CLAUDE WORKFLOW DENIAL REPORT", "# END CLAUDE WORKFLOW DENIAL REPORT")

RESULT = {"type": "result", "subtype": "error_max_turns", "num_turns": 81,
          "duration_ms": 929526, "permission_denials": []}


class ClaudeStallReportTests(unittest.TestCase):
    """Covers the Issue-targeted report posted when the Claude step itself did not succeed."""

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.record = self.root / "claude-execution-output.json"
        self.report = self.root / "report.json"

    def run_code(self, *, data=None, raw=None, outcome="failure", issue_number="83",
                 repository="hide212131/hane", run_id="123"):
        if raw is not None:
            self.record.write_text(raw)
        elif data is not None:
            self.record.write_text(json.dumps(data))
        env = dict(os.environ, RUNNER_TEMP=str(self.root), EXECUTION_FILE=str(self.record) if (data or raw) else "",
                   CLAUDE_OUTCOME=outcome, ISSUE_NUMBER=issue_number, REPOSITORY=repository,
                   RUN_ID=run_id, REPORT_FILE=str(self.report))
        process = subprocess.run([sys.executable, "-I", "-c", STALL_CODE], env=env,
                                  capture_output=True, text=True, timeout=5)
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(process.stdout, "")
        self.assertEqual(process.stderr, "")
        return json.loads(self.report.read_text())

    def test_failure_without_execution_record_has_generic_reason(self):
        report = self.run_code(outcome="failure")
        self.assertTrue(report["should_post"])
        self.assertIn("hane-stall: number=83 stage=implement sha=none", report["marker"])
        self.assertIn("Claude did not report a specific reason.", report["body"])
        self.assertIn("Target: Issue #83", report["body"])
        self.assertIn("https://github.com/hide212131/hane/actions/runs/123", report["body"])

    def test_known_subtype_maps_to_specific_reason(self):
        report = self.run_code(data=RESULT, outcome="failure")
        self.assertIn("Claude reached its turn limit before finishing the implementation.", report["body"])

    def test_cancelled_outcome_has_its_own_reason(self):
        report = self.run_code(outcome="cancelled")
        self.assertIn("cancelled before Claude finished", report["body"])

    def test_success_is_never_reached_but_missing_env_suppresses_posting(self):
        report = self.run_code(outcome="failure", issue_number="")
        self.assertFalse(report["should_post"])

    def test_untrusted_subtype_is_ignored(self):
        result = dict(RESULT, subtype="SECRET-EXAMPLE\n::warning::x")
        report = self.run_code(data=result, outcome="failure")
        self.assertNotIn("SECRET-EXAMPLE", report["body"])
        self.assertNotIn("::warning::", report["body"])

    def test_invalid_json_falls_back_to_generic_reason(self):
        report = self.run_code(raw="not-json SECRET-EXAMPLE", outcome="failure")
        self.assertNotIn("SECRET-EXAMPLE", report["body"])
        self.assertIn("Claude did not report a specific reason.", report["body"])


class ClaudeWorkflowDenialReportTests(unittest.TestCase):
    """Covers the PR-targeted report for workflow-file writes Claude's App could not push."""

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.record = self.root / "claude-execution-output.json"
        self.report = self.root / "report.json"

    def run_code(self, *, data, pr_number="81", target_sha="e3144213abcd", repository="hide212131/hane", run_id="123"):
        self.record.write_text(json.dumps(data))
        env = dict(os.environ, RUNNER_TEMP=str(self.root), EXECUTION_FILE=str(self.record),
                   PR_NUMBER=pr_number, TARGET_SHA=target_sha, REPOSITORY=repository,
                   RUN_ID=run_id, REPORT_FILE=str(self.report))
        process = subprocess.run([sys.executable, "-I", "-c", DENIAL_CODE], env=env,
                                  capture_output=True, text=True, timeout=5)
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(process.stdout, "")
        self.assertEqual(process.stderr, "")
        return json.loads(self.report.read_text())

    def test_denied_workflow_write_is_reported(self):
        result = dict(RESULT, permission_denials=[
            {"tool_name": "Write", "tool_input": {"file_path": ".github/workflows/codex-review-reconcile.yml"}},
        ])
        report = self.run_code(data=result)
        self.assertTrue(report["should_post"])
        self.assertIn("`.github/workflows/codex-review-reconcile.yml`", report["body"])
        self.assertEqual(report["marker"], "hane-stall: number=81 stage=implement-workflow-denied sha=e3144213abcd")
        self.assertIn("Target: PR #81 / e3144213abcd", report["body"])

    def test_non_workflow_denials_are_not_reported(self):
        result = dict(RESULT, permission_denials=[
            {"tool_name": "Write", "tool_input": {"file_path": "crates/app/src/main.rs"}},
            {"tool_name": "Bash", "tool_input": {"command": "rm -rf /"}},
        ])
        report = self.run_code(data=result)
        self.assertFalse(report["should_post"])

    def test_path_traversal_and_injection_attempts_are_rejected(self):
        result = dict(RESULT, permission_denials=[
            {"tool_name": "Write", "tool_input": {"file_path": "../.github/workflows/evil.yml"}},
            {"tool_name": "Write", "tool_input": {"file_path": ".github/workflows/../../etc/passwd"}},
            {"tool_name": "Write", "tool_input": {"file_path": ".github/workflows/`rm -rf /`.yml"}},
        ])
        report = self.run_code(data=result)
        self.assertFalse(report["should_post"])

    def test_duplicate_paths_are_deduplicated_and_capped(self):
        denials = [{"tool_name": "Write", "tool_input": {"file_path": ".github/workflows/a.yml"}}] * 3
        denials += [{"tool_name": "Edit", "tool_input": {"file_path": f".github/workflows/f{i}.yml"}} for i in range(30)]
        report = self.run_code(data=dict(RESULT, permission_denials=denials))
        self.assertEqual(report["body"].count(".github/workflows/a.yml"), 1)
        self.assertLessEqual(report["body"].count(".yml`"), 20)

    def test_missing_execution_record_suppresses_posting(self):
        self.record.unlink()
        env = dict(os.environ, RUNNER_TEMP=str(self.root), EXECUTION_FILE=str(self.record),
                   PR_NUMBER="81", TARGET_SHA="deadbeef", REPOSITORY="hide212131/hane",
                   RUN_ID="123", REPORT_FILE=str(self.report))
        process = subprocess.run([sys.executable, "-I", "-c", DENIAL_CODE], env=env,
                                  capture_output=True, text=True, timeout=5)
        self.assertEqual(process.returncode, 0, process.stderr)
        report = json.loads(self.report.read_text())
        self.assertFalse(report["should_post"])


class WorkflowWiringTests(unittest.TestCase):
    """Confirms the new steps are wired with safe conditions and the right permissions."""

    def test_stall_report_step_is_not_gated_on_existing_pr_skip_path(self):
        step = WORKFLOW.split("      - name: Report Claude stall to the Issue\n")[1].split("      - name:", 1)[0]
        self.assertIn("always() && steps.existing.outputs.skip != 'true' && steps.claude.outcome != 'success'", step)
        self.assertIn("continue-on-error: true", step)

    def test_workflow_denial_step_only_runs_once_pr_head_is_known(self):
        step = WORKFLOW.split("      - name: Report blocked workflow file changes to the Pull Request\n")[1].split("      - name:", 1)[0]
        self.assertIn("steps.target.outputs.target_sha != ''", step)
        self.assertIn("continue-on-error: true", step)

    def test_dedup_marker_is_used_for_both_new_steps(self):
        self.assertIn('--jq --arg marker "$marker"', WORKFLOW)
        self.assertEqual(WORKFLOW.count("gh api --method PATCH \"repos/${REPOSITORY}/issues/comments/${existing_id}\""), 2)


if __name__ == "__main__":
    unittest.main()
