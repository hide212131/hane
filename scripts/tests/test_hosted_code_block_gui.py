#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "hosted_code_block_gui.py"
SPEC = importlib.util.spec_from_file_location("hosted_code_block_gui", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
mod = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(mod)


class FixtureTests(unittest.TestCase):
    def test_opening_offset_points_at_hidden_fence(self):
        self.assertEqual(
            mod.FIXTURE_ORIGINAL[
                mod.OPENING_FENCE_OFFSET:mod.OPENING_FENCE_OFFSET + 7
            ],
            "~~~rust",
        )

    def test_edit_adds_exactly_one_fence_marker(self):
        expected = mod.FIXTURE_ORIGINAL.replace("~~~rust", "~~~~rust", 1)
        self.assertEqual(mod.FIXTURE_EDITED, expected)
        self.assertEqual(
            len(mod.FIXTURE_EDITED),
            len(mod.FIXTURE_ORIGINAL) + 1,
        )

    def test_procedure_is_focused_version_one(self):
        self.assertEqual(mod.PROCEDURE_VERSION, "hosted-code-block/1")
        self.assertEqual(mod.VERIFICATION_KIND, "code_block_focused")


class OcrEvaluationTests(unittest.TestCase):
    def test_accepts_required_visible_text(self):
        text = (
            "Neutral anchor\n"
            "rust\n"
            "let answer = 42;\n"
            "literal markdown\n"
            "Tail anchor"
        )
        result = mod.evaluate_initial_ocr(text)
        self.assertEqual(result["result"], "pass")
        self.assertEqual(result["missing"], [])

    def test_rejects_missing_language_label_or_code_content(self):
        result = mod.evaluate_initial_ocr("Neutral anchor\nTail anchor")
        self.assertEqual(result["result"], "fail")
        self.assertIn("rust", result["missing"])
        self.assertIn("let answer = 42", result["missing"])

    def test_ocr_check_is_whitespace_and_case_tolerant_only(self):
        text = (
            "NEUTRAL   ANCHOR\n"
            "RUST\n"
            "LET ANSWER = 42\n"
            "LITERAL MARKDOWN\n"
            "TAIL ANCHOR"
        )
        self.assertEqual(mod.evaluate_initial_ocr(text)["result"], "pass")


if __name__ == "__main__":
    unittest.main()
