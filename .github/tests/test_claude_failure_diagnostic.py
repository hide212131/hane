import importlib.util
import io
import json
from contextlib import redirect_stdout
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "classify_claude_failure", ROOT / "scripts/classify_claude_failure.py"
)
classifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(classifier)

WORKFLOW = (ROOT / "workflows/claude-fix.yml").read_text()


class ClaudeFailureClassifierTests(unittest.TestCase):
    def test_rate_limit_event_wins_without_printing_raw_content(self):
        payload = [
            {"type": "rate_limit_event", "rate_limit_info": {"status": "rejected"}},
            {
                "type": "result",
                "subtype": "success",
                "is_error": True,
                "result": "secret-reset-message-do-not-print",
                "num_turns": 20,
                "total_cost_usd": 0.9,
                "modelUsage": {"claude": {}},
            },
        ]
        self.assertEqual(
            classifier.classify_events(payload), classifier.CATEGORY_USAGE
        )
        summary = classifier.format_summary(
            classifier.diagnostic(payload), execution_file="present"
        )
        self.assertNotIn("secret-reset-message-do-not-print", summary)

    def test_authentication_is_classified_from_error_result_only(self):
        payload = [
            {
                "type": "result",
                "subtype": "success",
                "is_error": True,
                "result": "API Error: HTTP 401 unauthorized oauth token",
                "num_turns": 1,
                "total_cost_usd": 0,
                "modelUsage": {},
            }
        ]
        self.assertEqual(
            classifier.classify_events(payload), classifier.CATEGORY_AUTH
        )

    def test_max_turns_uses_subtype(self):
        payload = [
            {
                "type": "result",
                "subtype": "error_max_turns",
                "is_error": True,
                "num_turns": 160,
                "total_cost_usd": 1.0,
                "modelUsage": {"claude": {}},
            }
        ]
        self.assertEqual(
            classifier.classify_events(payload), classifier.CATEGORY_MAX_TURNS
        )

    def test_model_provider_error_is_classified(self):
        payload = [
            {
                "type": "result",
                "subtype": "success",
                "is_error": True,
                "result": "API Error: model not found",
                "num_turns": 1,
                "total_cost_usd": 0,
                "modelUsage": {},
            }
        ]
        self.assertEqual(
            classifier.classify_events(payload),
            classifier.CATEGORY_MODEL_PROVIDER,
        )

    def test_zero_cost_shape_is_not_mislabeled_as_usage_limit(self):
        payload = [
            {
                "type": "result",
                "subtype": "success",
                "is_error": True,
                "num_turns": 1,
                "total_cost_usd": 0,
                "modelUsage": {},
            }
        ]
        self.assertEqual(
            classifier.classify_events(payload), classifier.CATEGORY_ZERO_COST
        )

    def test_unknown_failure_stays_unknown(self):
        payload = [
            {
                "type": "result",
                "subtype": "success",
                "is_error": True,
                "num_turns": 3,
                "total_cost_usd": 0.1,
                "modelUsage": {"claude": {}},
            }
        ]
        self.assertEqual(
            classifier.classify_events(payload), classifier.CATEGORY_UNKNOWN
        )

    def test_missing_execution_file_reports_only_static_metadata(self):
        out = io.StringIO()
        with redirect_stdout(out):
            self.assertEqual(classifier.main(["classifier", "/does/not/exist"]), 0)
        text = out.getvalue()
        self.assertIn("category=diagnostic_unavailable", text)
        self.assertIn("execution_file=missing", text)

    def test_json_file_diagnostic_never_echoes_error_text(self):
        payload = [
            {
                "type": "result",
                "subtype": "success",
                "is_error": True,
                "result": "API Error: HTTP 401 secret-value-123",
                "num_turns": 1,
                "total_cost_usd": 0,
                "modelUsage": {},
            }
        ]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "execution.json"
            path.write_text(json.dumps(payload), encoding="utf-8")
            out = io.StringIO()
            with redirect_stdout(out):
                self.assertEqual(classifier.main(["classifier", str(path)]), 0)
        text = out.getvalue()
        self.assertIn("category=authentication", text)
        self.assertNotIn("secret-value-123", text)


class ClaudeFailureWorkflowWiringTests(unittest.TestCase):
    def test_failure_is_diagnosed_then_preserved_as_job_failure(self):
        self.assertIn(
            'id: claude\n        continue-on-error: true\n        uses: anthropics/claude-code-base-action@',
            WORKFLOW,
        )
        self.assertIn(
            "if: steps.claude.outcome == 'failure'",
            WORKFLOW,
        )
        self.assertIn(
            "ref: ${{ github.workflow_sha }}",
            WORKFLOW,
        )
        self.assertIn(
            "path: trusted-diagnostic",
            WORKFLOW,
        )
        self.assertIn(
            "python trusted-diagnostic/.github/scripts/classify_claude_failure.py",
            WORKFLOW,
        )
        self.assertIn(
            'candidate="$RUNNER_TEMP/claude-execution-output.json"',
            WORKFLOW,
        )
        diagnostic_block = WORKFLOW.split(
            "      - name: Diagnose Claude execution failure\n", 1
        )[1].split("\n      - name: Build untrusted worker patch", 1)[0]
        self.assertIn("exit 1", diagnostic_block)

    def test_diagnostic_classifier_is_reacquired_after_claude(self):
        claude_index = WORKFLOW.index("id: claude")
        checkout_index = WORKFLOW.index(
            "Check out trusted diagnostic source after Claude failure"
        )
        diagnose_index = WORKFLOW.index("Diagnose Claude execution failure")
        self.assertLess(claude_index, checkout_index)
        self.assertLess(checkout_index, diagnose_index)

    def test_full_output_is_not_enabled(self):
        self.assertNotIn("show_full_output: true", WORKFLOW)


if __name__ == "__main__":
    unittest.main()
