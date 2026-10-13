import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("verify_claude_model", ROOT / "scripts/verify_claude_model.py")
verify_claude_model = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify_claude_model)


def execution(model="claude-sonnet-5-5", usage_model="claude-sonnet-5-5"):
    return [
        {"type": "system", "subtype": "init", "model": model, "parent_tool_use_id": None},
        {"type": "result", "modelUsage": {usage_model: {"inputTokens": 1}}},
    ]


class ClaudeModelVerificationTests(unittest.TestCase):
    def test_accepts_requested_alias_and_dated_snapshot(self):
        for model in ("claude-sonnet-5-5", "claude-sonnet-5-5-20260928"):
            with self.subTest(model=model):
                result = verify_claude_model.verify_execution(execution(model, model))
                self.assertEqual(result["requested_model"], "claude-sonnet-5-5")
                self.assertEqual(result["session_model"], model)

    def test_rejects_old_or_different_session_model(self):
        for model in ("claude-sonnet-5", "claude-opus-5-5", "claude-sonnet-5-5-beta"):
            with self.subTest(model=model), self.assertRaises(ValueError):
                verify_claude_model.verify_execution(execution(model))

    def test_requires_result_usage_for_sonnet_55(self):
        with self.assertRaisesRegex(ValueError, "did not report Sonnet 5.5 model usage"):
            verify_claude_model.verify_execution(execution(usage_model="claude-sonnet-5"))

    def test_reports_all_runtime_usage_models(self):
        payload = execution()
        payload[1]["modelUsage"]["claude-haiku-4-5"] = {"inputTokens": 1}
        result = verify_claude_model.verify_execution(payload)
        self.assertEqual(result["usage_models"], ["claude-haiku-4-5", "claude-sonnet-5-5"])

    def test_requires_one_primary_session_model(self):
        payload = execution()
        payload.append(payload[0].copy())
        with self.assertRaisesRegex(ValueError, "did not report the configured"):
            verify_claude_model.verify_execution(payload)

    def test_reads_only_bounded_regular_execution_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            original = root / "execution.json"
            original.write_text("[]", encoding="utf-8")
            link = root / "linked.json"
            link.symlink_to(original)
            with self.assertRaisesRegex(ValueError, "bounded regular file"):
                verify_claude_model.read_execution(link)


if __name__ == "__main__":
    unittest.main()
