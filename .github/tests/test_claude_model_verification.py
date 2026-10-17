import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("verify_claude_model", ROOT / "scripts/verify_claude_model.py")
verify_claude_model = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify_claude_model)


def execution(model="claude-sonnet-5-5", usage_model="claude-sonnet-5-5", usage=None):
    if usage is None:
        usage = {"inputTokens": 1}
    return [
        {"type": "system", "subtype": "init", "model": model, "parent_tool_use_id": None},
        {"type": "result", "modelUsage": {usage_model: usage}},
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

    def test_rejects_missing_primary_session_model(self):
        payload = execution()
        payload[0] = {"type": "system", "subtype": "init", "parent_tool_use_id": None}
        with self.assertRaisesRegex(ValueError, "did not report the configured"):
            verify_claude_model.verify_execution(payload)

    def test_requires_result_usage_for_sonnet_55(self):
        with self.assertRaisesRegex(ValueError, "did not report positive Sonnet 5.5 token usage"):
            verify_claude_model.verify_execution(execution(usage_model="claude-sonnet-5"))

    def test_requires_positive_sonnet_55_token_usage(self):
        token_fields = (
            "inputTokens",
            "outputTokens",
            "cacheReadInputTokens",
            "cacheCreationInputTokens",
        )
        for field in token_fields:
            with self.subTest(field=field):
                usage = {name: 0 for name in token_fields}
                usage[field] = 1
                result = verify_claude_model.verify_execution(execution(usage=usage))
                self.assertEqual(result["session_model"], "claude-sonnet-5-5")

        for usage in (
            {},
            {field: 0 for field in token_fields},
            {field: -1 for field in token_fields},
            {field: True for field in token_fields},
        ):
            with self.subTest(usage=usage), self.assertRaisesRegex(
                ValueError, "positive Sonnet 5.5 token usage"
            ):
                verify_claude_model.verify_execution(execution(usage=usage))

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

    def test_reads_a_valid_execution_file_larger_than_the_old_bound(self):
        old_bound = 16 * 1024 * 1024
        self.assertEqual(verify_claude_model.MAX_EXECUTION_BYTES, 64 * 1024 * 1024)
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "execution.json"
            payload = execution()
            payload.insert(1, {"type": "user", "message": {"content": "x" * (old_bound + 1024)}})
            encoded = json.dumps(payload, separators=(",", ":"))
            size = len(encoded.encode("utf-8"))
            self.assertGreater(size, old_bound)
            self.assertLessEqual(size, verify_claude_model.MAX_EXECUTION_BYTES)
            path.write_text(encoded, encoding="utf-8")

            result = verify_claude_model.read_execution(path)
            self.assertEqual(result["session_model"], "claude-sonnet-5-5")

    def test_rejects_missing_non_regular_symlink_and_oversized_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            missing = root / "missing.json"
            directory = root / "directory"
            directory.mkdir()
            original = root / "execution.json"
            original.write_text(json.dumps(execution()), encoding="utf-8")
            link = root / "linked.json"
            link.symlink_to(original)
            oversized = root / "oversized.json"
            with oversized.open("wb") as file:
                file.truncate(verify_claude_model.MAX_EXECUTION_BYTES + 1)

            for path in (missing, directory, link, oversized):
                with self.subTest(path=path.name), self.assertRaisesRegex(
                    ValueError, "bounded regular file"
                ):
                    verify_claude_model.read_execution(path)

    def test_rejects_malformed_execution_file(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "execution.json"
            path.write_text("{", encoding="utf-8")
            with self.assertRaises(json.JSONDecodeError):
                verify_claude_model.read_execution(path)


if __name__ == "__main__":
    unittest.main()
