"""Tests for aadw_jev_contract.py: fixture-only, no network, no SDK dependency.

These tests check the fixed-data request/result contract shape described in
docs/aadw-v3-v3-2-validation.md. They never call the Jev API or import
@typesafe-ai/sdk; a pass here is not evidence that a real service connection
succeeds.
"""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import aadw_jev_contract as contract


def valid_request() -> dict:
    return {
        "state": {"topic": "example"},
        "questions": {
            "sentiment": {"type": "noul"},
            "category": {
                "type": "choice",
                "criteria": {"positive": "positive tone", "negative": "negative tone", "neutral": "neutral tone"},
            },
        },
        "model": "system-one-default",
    }


def valid_result() -> dict:
    return {
        "model": "system-one-default",
        "answers": {
            "sentiment": {"type": "noul", "noul": 0.73},
            "category": {
                "type": "choice",
                "choice": "positive",
                "confidence": 0.9,
                "probabilities": {"positive": 0.9, "negative": 0.05, "neutral": 0.05},
            },
        },
        "usage": {"input_tokens": 120, "output_tokens": 40},
    }


class ValidPairTests(unittest.TestCase):
    def test_valid_choice_and_noul_pair_passes(self):
        contract.check(valid_request(), valid_result())  # must not raise

    def test_valid_pair_passes_as_json_text(self):
        contract.check(json.dumps(valid_request()), json.dumps(valid_result()))

    def test_request_without_optional_model_passes(self):
        request = valid_request()
        del request["model"]
        contract.check(request, valid_result())


class MalformedJsonTests(unittest.TestCase):
    def test_invalid_request_json_is_rejected(self):
        with self.assertRaises(contract.ContractError):
            contract.check("{not valid json", json.dumps(valid_result()))

    def test_invalid_result_json_is_rejected(self):
        with self.assertRaises(contract.ContractError):
            contract.check(json.dumps(valid_request()), "{not valid json")

    def test_non_object_request_is_rejected(self):
        with self.assertRaises(contract.ContractError):
            contract.check("[1, 2, 3]", json.dumps(valid_result()))

    def test_non_object_result_is_rejected(self):
        with self.assertRaises(contract.ContractError):
            contract.check(json.dumps(valid_request()), "42")


class RequestShapeTests(unittest.TestCase):
    def test_empty_questions_is_rejected(self):
        request = valid_request()
        request["questions"] = {}
        with self.assertRaises(contract.ContractError):
            contract.check(request, valid_result())

    def test_missing_questions_is_rejected(self):
        request = valid_request()
        del request["questions"]
        with self.assertRaises(contract.ContractError):
            contract.check(request, valid_result())

    def test_missing_state_is_rejected(self):
        request = valid_request()
        del request["state"]
        with self.assertRaises(contract.ContractError):
            contract.check(request, valid_result())

    def test_choice_question_without_criteria_is_rejected(self):
        request = valid_request()
        del request["questions"]["category"]["criteria"]
        with self.assertRaises(contract.ContractError):
            contract.check(request, valid_result())

    def test_unknown_question_type_is_rejected(self):
        request = valid_request()
        request["questions"]["sentiment"]["type"] = "score"
        with self.assertRaises(contract.ContractError):
            contract.check(request, valid_result())


class ResultShapeTests(unittest.TestCase):
    def test_empty_model_is_rejected(self):
        result = valid_result()
        result["model"] = ""
        with self.assertRaises(contract.ContractError):
            contract.check(valid_request(), result)

    def test_missing_model_is_rejected(self):
        result = valid_result()
        del result["model"]
        with self.assertRaises(contract.ContractError):
            contract.check(valid_request(), result)

    def test_missing_answer_key_is_rejected(self):
        result = valid_result()
        del result["answers"]["sentiment"]
        with self.assertRaises(contract.ContractError):
            contract.check(valid_request(), result)

    def test_answer_type_mismatch_is_rejected(self):
        result = valid_result()
        result["answers"]["sentiment"]["type"] = "choice"
        with self.assertRaises(contract.ContractError):
            contract.check(valid_request(), result)


class UsageTests(unittest.TestCase):
    def test_negative_usage_is_rejected(self):
        result = valid_result()
        result["usage"]["input_tokens"] = -1
        with self.assertRaises(contract.ContractError):
            contract.check(valid_request(), result)

    def test_non_integer_usage_is_rejected(self):
        result = valid_result()
        result["usage"]["output_tokens"] = 3.5
        with self.assertRaises(contract.ContractError):
            contract.check(valid_request(), result)

    def test_missing_usage_field_is_rejected(self):
        result = valid_result()
        del result["usage"]["input_tokens"]
        with self.assertRaises(contract.ContractError):
            contract.check(valid_request(), result)

    def test_missing_usage_object_is_rejected(self):
        result = valid_result()
        del result["usage"]
        with self.assertRaises(contract.ContractError):
            contract.check(valid_request(), result)

    def test_bool_usage_is_not_accepted_as_integer(self):
        # bool is an int subclass in Python; true/false must not pass as counts.
        result = valid_result()
        result["usage"]["input_tokens"] = True
        with self.assertRaises(contract.ContractError):
            contract.check(valid_request(), result)


class NoulAnswerTests(unittest.TestCase):
    def test_out_of_range_values_are_rejected(self):
        for value in (-0.1, 1.1, -1, 2):
            with self.subTest(value=value):
                result = valid_result()
                result["answers"]["sentiment"]["noul"] = value
                with self.assertRaises(contract.ContractError):
                    contract.check(valid_request(), result)

    def test_nonfinite_literal_json_values_are_rejected(self):
        # json.loads accepts NaN/Infinity/-Infinity as a non-standard extension;
        # the contract must reject them explicitly rather than trust isinstance.
        for literal in ("NaN", "Infinity", "-Infinity"):
            with self.subTest(literal=literal):
                result = valid_result()
                result_text = json.dumps(result).replace('"noul": 0.73', f'"noul": {literal}')
                with self.assertRaises(contract.ContractError):
                    contract.check(json.dumps(valid_request()), result_text)


class ChoiceAnswerTests(unittest.TestCase):
    def test_choice_outside_criteria_is_rejected(self):
        result = valid_result()
        result["answers"]["category"]["choice"] = "unknown-label"
        with self.assertRaises(contract.ContractError):
            contract.check(valid_request(), result)

    def test_confidence_out_of_range_or_nonfinite_is_rejected(self):
        for value in (-0.1, 1.1, float("nan"), float("inf")):
            with self.subTest(value=value):
                result = valid_result()
                result["answers"]["category"]["confidence"] = value
                with self.assertRaises(contract.ContractError):
                    contract.check(valid_request(), result)

    def test_probability_out_of_range_or_nonfinite_is_rejected(self):
        for value in (-0.1, 1.1, float("nan"), float("inf")):
            with self.subTest(value=value):
                result = valid_result()
                result["answers"]["category"]["probabilities"]["positive"] = value
                with self.assertRaises(contract.ContractError):
                    contract.check(valid_request(), result)

    def test_missing_probability_key_is_rejected(self):
        result = valid_result()
        del result["answers"]["category"]["probabilities"]["neutral"]
        with self.assertRaises(contract.ContractError):
            contract.check(valid_request(), result)

    def test_extra_probability_key_is_rejected(self):
        result = valid_result()
        result["answers"]["category"]["probabilities"]["extra-label"] = 0.0
        with self.assertRaises(contract.ContractError):
            contract.check(valid_request(), result)


class CliTests(unittest.TestCase):
    def _write(self, directory: Path, name: str, payload) -> str:
        path = directory / name
        text = payload if isinstance(payload, str) else json.dumps(payload)
        path.write_text(text, encoding="utf-8")
        return str(path)

    def test_valid_pair_exits_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            request_path = self._write(directory, "request.json", valid_request())
            result_path = self._write(directory, "result.json", valid_result())
            with contextlib.redirect_stdout(io.StringIO()):
                code = contract.main(["aadw_jev_contract.py", request_path, result_path])
        self.assertEqual(code, contract.EXIT_OK)

    def test_contract_violation_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            broken_result = valid_result()
            broken_result["model"] = ""
            request_path = self._write(directory, "request.json", valid_request())
            result_path = self._write(directory, "result.json", broken_result)
            with contextlib.redirect_stderr(io.StringIO()) as error:
                code = contract.main(["aadw_jev_contract.py", request_path, result_path])
        self.assertEqual(code, contract.EXIT_VIOLATION)
        self.assertIn("contract violation", error.getvalue())

    def test_wrong_argument_count_exits_usage(self):
        with contextlib.redirect_stderr(io.StringIO()):
            code = contract.main(["aadw_jev_contract.py", "only-one-arg.json"])
        self.assertEqual(code, contract.EXIT_USAGE)

    def test_missing_file_exits_usage(self):
        with contextlib.redirect_stderr(io.StringIO()):
            code = contract.main(["aadw_jev_contract.py", "missing-a.json", "missing-b.json"])
        self.assertEqual(code, contract.EXIT_USAGE)


if __name__ == "__main__":
    unittest.main()
