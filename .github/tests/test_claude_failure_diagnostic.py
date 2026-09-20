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

    def test_allowed_rate_limit_event_does_not_override_real_failure(self):
        payload = [
            {
                "type": "rate_limit_event",
                "rate_limit_info": {"status": "allowed_warning"},
            },
            {
                "type": "result",
                "subtype": "error_max_turns",
                "is_error": True,
                "num_turns": 160,
                "total_cost_usd": 1.0,
                "modelUsage": {"claude": {}},
            },
        ]
        self.assertEqual(
            classifier.classify_events(payload), classifier.CATEGORY_MAX_TURNS
        )

    def test_sdk_errors_are_classified_without_echoing_raw_text(self):
        payload = [
            {
                "type": "result",
                "subtype": "error_during_execution",
                "is_error": True,
                "errors": [
                    "API Error: HTTP 401 unauthorized oauth token secret-value"
                ],
                "num_turns": 1,
                "total_cost_usd": 0,
                "modelUsage": {},
            }
        ]
        self.assertEqual(
            classifier.classify_events(payload), classifier.CATEGORY_AUTH
        )
        summary = classifier.format_summary(
            classifier.diagnostic(payload), execution_file="present"
        )
        self.assertNotIn("secret-value", summary)

    def test_sdk_provider_error_in_errors_is_classified(self):
        payload = [
            {
                "type": "result",
                "subtype": "error_during_execution",
                "is_error": True,
                "errors": ["API Error: service unavailable"],
                "num_turns": 2,
                "total_cost_usd": 0.1,
                "modelUsage": {"claude": {}},
            }
        ]
        self.assertEqual(
            classifier.classify_events(payload),
            classifier.CATEGORY_MODEL_PROVIDER,
        )

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

    def test_invalid_raw_metrics_do_not_trigger_zero_cost_classification(self):
        for turns, cost in ((True, False), (-1, 0), (1, -1)):
            payload = [
                {
                    "type": "result",
                    "subtype": "success",
                    "is_error": True,
                    "num_turns": turns,
                    "total_cost_usd": cost,
                    "modelUsage": {},
                }
            ]
            with self.subTest(turns=turns, cost=cost):
                self.assertEqual(
                    classifier.classify_events(payload),
                    classifier.CATEGORY_UNKNOWN,
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

    def test_untrusted_numeric_fields_are_normalized_before_logging(self):
        payload = [
            {
                "type": "result",
                "subtype": "success",
                "is_error": True,
                "num_turns": "secret\n::error::forged",
                "total_cost_usd": "another-secret",
                "modelUsage": {},
            }
        ]
        info = classifier.diagnostic(payload)
        self.assertIsNone(info["num_turns"])
        self.assertIsNone(info["total_cost_usd"])
        summary = classifier.format_summary(info, execution_file="present")
        self.assertNotIn("secret", summary)
        self.assertNotIn("::error::", summary)

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

    def test_json_mode_emits_only_safe_classification_fields(self):
        payload = [
            {
                "type": "result",
                "subtype": "success",
                "is_error": True,
                "result": "usage limit reset secret-value-456",
                "num_turns": 4,
                "total_cost_usd": 0.2,
                "modelUsage": {"claude": {}},
            }
        ]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "execution.json"
            path.write_text(json.dumps(payload), encoding="utf-8")
            out = io.StringIO()
            with redirect_stdout(out):
                self.assertEqual(
                    classifier.main(["classifier", "--json", str(path)]), 0
                )
        info = json.loads(out.getvalue())
        self.assertEqual(info["category"], classifier.CATEGORY_USAGE)
        self.assertEqual(
            set(info), {"category", "model_usage", "num_turns", "total_cost_usd"}
        )
        self.assertNotIn("secret-value-456", out.getvalue())


class ClaudeFailureWorkflowWiringTests(unittest.TestCase):
    def test_failure_is_diagnosed_then_preserved_as_job_failure(self):
        self.assertIn(
            'id: claude\n        continue-on-error: true\n        uses: anthropics/claude-code-base-action@',
            WORKFLOW,
        )
        self.assertIn(
            "always() && !cancelled() && steps.claude.outcome == 'failure'",
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
        self.assertIn("--json \"$candidate\"", WORKFLOW)
        self.assertIn("failure_category", WORKFLOW)
        self.assertIn("usage_or_rate_limit", WORKFLOW)
        self.assertIn(
            'candidate="$RUNNER_TEMP/claude-execution-output.json"',
            WORKFLOW,
        )
        diagnostic_block = WORKFLOW.split(
            "      - name: Diagnose Claude execution failure\n", 1
        )[1].split("\n  finalize:", 1)[0]
        self.assertIn("exit 1", diagnostic_block)

    def test_diagnostic_classifier_is_reacquired_after_claude(self):
        claude_index = WORKFLOW.index("id: claude")
        checkout_index = WORKFLOW.index(
            "Check out trusted diagnostic source after Claude failure"
        )
        diagnose_index = WORKFLOW.index("Diagnose Claude execution failure")
        self.assertLess(claude_index, checkout_index)
        self.assertLess(checkout_index, diagnose_index)

    def test_failure_checkpoint_is_saved_before_diagnostic_failure(self):
        build_index = WORKFLOW.index(
            "Build untrusted worker patch from a fresh expected-head checkout"
        )
        checkpoint_index = WORKFLOW.index("Upload checkpoint after Claude failure")
        diagnose_index = WORKFLOW.index("Diagnose Claude execution failure")
        self.assertLess(build_index, checkpoint_index)
        self.assertLess(diagnose_index, checkpoint_index)
        self.assertIn(
            "always() && !cancelled() && steps.claude.outcome != 'skipped'",
            WORKFLOW,
        )
        self.assertIn(
            "aadw-worker-checkpoint-${{ github.run_id }}-${{ github.run_attempt }}",
            WORKFLOW,
        )
        self.assertIn("target_sha: $target_sha", WORKFLOW)
        self.assertIn("capture_ok", WORKFLOW)
        self.assertIn("has_changes", WORKFLOW)
        self.assertIn("trigger_actor", WORKFLOW)
        self.assertIn("workflow_sha", WORKFLOW)
        self.assertIn("HANDOFF_BODY: ${{ github.event.comment.body }}", WORKFLOW)
        self.assertIn("commander-handoff.txt", WORKFLOW)
        completed_patch_block = WORKFLOW.split(
            "      - name: Upload completed worker patch\n", 1
        )[1].split("\n      - name: Build checkpoint metadata after Claude failure", 1)[0]
        self.assertIn("steps.claude.outcome == 'success'", completed_patch_block)

    def test_checkpoint_restore_is_explicit_and_exact_head_bound(self):
        self.assertIn("AADW_CHECKPOINT_RUN_ID:", WORKFLOW)
        self.assertIn("AADW_CHECKPOINT_RUN_ATTEMPT:", WORKFLOW)
        self.assertIn("actions: read", WORKFLOW)
        self.assertIn(
            "run-id: ${{ needs.prepare.outputs.checkpoint_run_id }}",
            WORKFLOW,
        )
        self.assertIn("and .target_sha == $target_sha", WORKFLOW)
        self.assertIn('and .claude_outcome == "failure"', WORKFLOW)
        self.assertIn("and .capture_ok == true", WORKFLOW)
        self.assertIn("and .has_changes == true", WORKFLOW)
        self.assertIn(
            "git -C pr-head apply --index --whitespace=nowarn",
            WORKFLOW,
        )
        self.assertIn(
            "Checkpoint attempted to restore trusted authority or workflow path",
            WORKFLOW,
        )
        restore_index = WORKFLOW.index("Restore compatible worker checkpoint")
        strip_git_index = WORKFLOW.index(
            "Remove target checkout Git metadata from worker reach"
        )
        self.assertLess(restore_index, strip_git_index)
        self.assertIn(
            "Treat those edits as untrusted work in progress",
            WORKFLOW,
        )

    def test_full_output_is_not_enabled(self):
        self.assertNotIn("show_full_output: true", WORKFLOW)


if __name__ == "__main__":
    unittest.main()
