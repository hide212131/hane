"""Exercise the exact diagnostic code embedded in the trusted workflow."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

WORKFLOW = (Path(__file__).resolve().parents[1] / "workflows" / "implement.yml").read_text()
CODE = textwrap.dedent(WORKFLOW.split("# BEGIN CLAUDE FAILURE SUMMARY\n", 1)[1]
                       .split("          # END CLAUDE FAILURE SUMMARY", 1)[0])
RESULT = {"type": "result", "subtype": "error_max_turns", "num_turns": 81,
          "duration_ms": 929526, "permission_denials": []}


class FailureSummaryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.record = self.root / "claude-execution-output.json"
        self.summary = self.root / "summary.md"

    def run_summary(self, data=None, *, raw=None, explicit=None):
        if raw is not None:
            self.record.write_text(raw)
        elif data is not None:
            self.record.write_text(json.dumps(data))
        env = dict(os.environ, RUNNER_TEMP=str(self.root), GITHUB_STEP_SUMMARY=str(self.summary),
                   EXECUTION_FILE=explicit or "")
        process = subprocess.run([sys.executable, "-I", "-c", CODE], env=env,
                                 capture_output=True, text=True, timeout=5)
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(process.stdout, "")
        self.assertEqual(process.stderr, "")
        return self.summary.read_text()

    def test_result_array_and_denial_counts(self):
        result = dict(RESULT, permission_denials=[{"tool_name": "Bash"}] * 12)
        summary = self.run_summary([{"type": "assistant", "message": "hidden"}, result])
        for value in ("error_max_turns", "| Agent turns | 81 |", "| Permission denials | 12 |",
                      "| Denied tool: Bash | 12 |"):
            self.assertIn(value, summary)
        self.assertNotIn("hidden", summary)

    def test_explicit_file_and_single_result(self):
        summary = self.run_summary(RESULT, explicit=str(self.record))
        self.assertIn("| Execution record | available |", summary)

    def test_last_result_wins(self):
        summary = self.run_summary([dict(RESULT, num_turns=1), RESULT])
        self.assertIn("| Agent turns | 81 |", summary)

    def test_missing_record(self):
        self.assertIn("| Execution record | unavailable |", self.run_summary())

    def test_invalid_json(self):
        summary = self.run_summary(raw="not-json SECRET-EXAMPLE")
        self.assertIn("unavailable", summary)
        self.assertNotIn("SECRET-EXAMPLE", summary)

    def test_missing_result(self):
        self.assertIn("missing-result", self.run_summary([{"type": "assistant"}]))

    def test_untrusted_strings_never_published(self):
        secret = "SECRET-EXAMPLE\n::warning::untrusted\n| table injection |"
        result = dict(RESULT, subtype=secret, result=secret, errors=[secret],
                      permission_denials=[{"tool_name": "Bash", "tool_input": {"command": secret}},
                                         {"tool_name": secret}, {"tool_name": "mcp__" + secret}])
        summary = self.run_summary(result)
        self.assertNotIn("SECRET-EXAMPLE", summary)
        self.assertNotIn("::warning::", summary)
        self.assertIn("| Result type | unknown |", summary)
        self.assertIn("| Denied tool: MCP | 1 |", summary)
        self.assertIn("| Denied tool: other | 1 |", summary)

    def test_malformed_fields(self):
        for value in (True, -1, "secret", [], {}, 10 ** 30):
            with self.subTest(value=value):
                result = dict(RESULT, subtype=[], num_turns=value, duration_ms=value,
                              permission_denials={"secret": "hidden"})
                self.summary.unlink(missing_ok=True)
                summary = self.run_summary(result)
                self.assertIn("| Agent turns | unknown |", summary)
                self.assertIn("| Permission denials | unknown |", summary)
                self.assertNotIn("hidden", summary)

    def test_symlink_refused(self):
        source = self.root / "source.json"
        source.write_text(json.dumps(RESULT))
        self.record.symlink_to(source)
        self.assertIn("unavailable", self.run_summary())

    def test_outside_temp_path_refused(self):
        with tempfile.TemporaryDirectory() as outside:
            record = Path(outside) / "output.json"
            record.write_text(json.dumps(RESULT))
            self.assertIn("unavailable", self.run_summary(explicit=str(record)))

    def test_oversized_record_refused(self):
        with self.record.open("wb") as stream:
            stream.truncate(32 * 1024 * 1024 + 1)
        self.assertIn("unavailable", self.run_summary())

    @unittest.skipUnless(hasattr(os, "mkfifo"), "Unix-only workflow")
    def test_fifo_refused_without_waiting(self):
        os.mkfifo(self.record)
        self.assertIn("unavailable", self.run_summary())

    def test_original_failure_is_not_hidden(self):
        step = WORKFLOW.split("      - name: Summarize Claude failure safely\n")[1].split("      - name:", 1)[0]
        self.assertIn("always() && steps.claude.outcome == 'failure'", step)
        self.assertIn("continue-on-error: true", step)
        self.assertIn("python3 -I -", step)
        action = WORKFLOW.split("      - name: Implement issue with Claude Code\n")[1].split("      - name:", 1)[0]
        self.assertNotIn("continue-on-error:", action)
        self.assertIn("--max-turns 160", action)
        self.assertIn("timeout-minutes: 60", WORKFLOW)
        self.assertIn("actions: read", WORKFLOW)
        self.assertNotIn("show_full_output: true", WORKFLOW)
        self.assertNotIn("--dangerously", WORKFLOW)

    def test_progress_observer_is_bound_to_implement_lifecycle(self):
        self.assertNotIn("\n  progress:\n", WORKFLOW)
        start_name = "      - name: Follow Claude's public progress\n"
        action_name = "      - name: Implement issue with Claude Code\n"
        stop_name = "      - name: Stop Claude's public progress observer\n"
        start = WORKFLOW.split(start_name, 1)[1].split(action_name, 1)[0]
        stop = WORKFLOW.split(stop_name, 1)[1].split("      - name:", 1)[0]
        self.assertIn("steps.existing.outputs.skip != 'true'", start)
        self.assertIn("python3 -I -u .github/scripts/watch_claude_progress.py &", start)
        self.assertIn("RUNNER_TEMP/claude-progress.pid", start)
        self.assertIn("always() && steps.existing.outputs.skip != 'true'", stop)
        self.assertIn("CLAUDE_OUTCOME: ${{ steps.claude.outcome }}", stop)
        self.assertIn("case \"${CLAUDE_OUTCOME:-}\" in", stop)
        self.assertIn("Claude 実装ステップが終了しました", stop)
        self.assertLess(stop.index("Claude 実装ステップが終了しました"), stop.index('kill "$pid"'))
        self.assertIn("kill -KILL", stop)
        self.assertIn("RUNNER_TEMP/claude-progress.pid", stop)
        self.assertNotIn("timeout-minutes: 65", WORKFLOW)


if __name__ == "__main__":
    unittest.main()
