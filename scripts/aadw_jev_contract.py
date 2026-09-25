#!/usr/bin/env python3
"""Fail-closed fixture contract check for Jev / TypeSafe System One (SDK 0.6.0).

This checks only the fixed shape of a System One request/result pair as
recorded in docs/aadw-v3-v3-2-validation.md: JSON structure, value ranges, and
the correspondence between a request's questions and a result's answers. It
does not call the Jev API or the @typesafe-ai/sdk package, and a passing check
here is not evidence that a real service connection succeeds.

EntryType (per the SDK): string | JSON object | JSON array | null. Values
nested inside an EntryType object/array follow plain JSON value rules
(string/number/boolean/null/array/object), so a number or bool is only valid
inside a nested object/array, never as the EntryType value itself. NaN/
Infinity/-Infinity are not JSON-compatible values anywhere in the structure.

Request shape: {"state": EntryType, "questions": {key: question, ...}, "model"?: str}
  - question is {"type": "noul", "instructions"?: EntryType,
                  "criteria"?: {"true"?: EntryType, "false"?: EntryType} | null}
    or {"type": "choice", "instructions"?: EntryType,
        "criteria": {label: EntryType, ...}}
Result shape: {"model": str, "answers": {key: answer, ...}, "usage": usage}
  - noul answer:   {"type": "noul", "noul": number in [0, 1]}
  - choice answer: {"type": "choice", "choice": label, "confidence": number in [0, 1],
                     "probabilities": {label: number in [0, 1], ...}}
  - usage: {"input_tokens": non-negative int, "output_tokens": non-negative int}
"""

from __future__ import annotations

import json
import math
import sys
from pathlib import Path
from typing import Any

EXIT_OK = 0
EXIT_VIOLATION = 1
EXIT_USAGE = 2


class ContractError(ValueError):
    """A request/result pair violates the fixed Jev/TypeSafe System One contract."""


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractError(message)


def _is_finite_number(value: Any) -> bool:
    # bool is an int subclass in Python and json permits NaN/Infinity as an
    # extension, so both must be excluded explicitly rather than relying on
    # isinstance(value, (int, float)) or a bare math.isfinite call.
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def _is_unit_interval(value: Any) -> bool:
    return _is_finite_number(value) and 0 <= value <= 1


def _is_nonnegative_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def _is_json_value(value: Any) -> bool:
    # Plain JSON value: string/number/boolean/null/array/object, with NaN/
    # Infinity/-Infinity excluded even though some JSON parsers accept them.
    if value is None or isinstance(value, str) or isinstance(value, bool):
        return True
    if isinstance(value, (int, float)):
        return _is_finite_number(value)
    if isinstance(value, list):
        return all(_is_json_value(item) for item in value)
    if isinstance(value, dict):
        return all(isinstance(key, str) and _is_json_value(item) for key, item in value.items())
    return False


def _is_entry_type(value: Any) -> bool:
    # EntryType: string | JSON object | JSON array | null. A bare number or
    # bool is not a valid EntryType value, only nested inside object/array.
    if value is None or isinstance(value, str):
        return True
    if isinstance(value, list):
        return all(_is_json_value(item) for item in value)
    if isinstance(value, dict):
        return all(isinstance(key, str) and _is_json_value(item) for key, item in value.items())
    return False


def parse_json(raw: Any, label: str) -> Any:
    if isinstance(raw, (str, bytes)):
        try:
            return json.loads(raw)
        except json.JSONDecodeError as exc:
            raise ContractError(f"{label} is not valid JSON: {exc}") from exc
    return raw


def validate_request(raw: Any) -> dict:
    request = parse_json(raw, "request")
    _require(isinstance(request, dict), "request must be a JSON object")
    _require("state" in request, "request.state is missing")
    _require(_is_entry_type(request.get("state")), "request.state must be an EntryType value")
    questions = request.get("questions")
    _require(isinstance(questions, dict) and len(questions) > 0,
             "request.questions must be a non-empty object")
    for key, question in questions.items():
        _require(isinstance(question, dict), f"request.questions[{key!r}] must be an object")
        question_type = question.get("type")
        _require(question_type in ("noul", "choice"),
                 f"request.questions[{key!r}].type must be 'noul' or 'choice'")
        if "instructions" in question:
            _require(_is_entry_type(question["instructions"]),
                     f"request.questions[{key!r}].instructions must be an EntryType value when present")
        if question_type == "noul":
            _validate_noul_criteria(key, question)
        else:
            _validate_choice_criteria(key, question)
    if "model" in request:
        _require(isinstance(request["model"], str), "request.model must be a string when present")
    return request


def _validate_noul_criteria(key: str, question: dict) -> None:
    if "criteria" not in question:
        return
    criteria = question["criteria"]
    _require(criteria is None or isinstance(criteria, dict),
             f"request.questions[{key!r}].criteria must be an object or null when present")
    if isinstance(criteria, dict):
        for branch in ("true", "false"):
            if branch in criteria:
                _require(_is_entry_type(criteria[branch]),
                         f"request.questions[{key!r}].criteria[{branch!r}] must be an EntryType value")


def _validate_choice_criteria(key: str, question: dict) -> None:
    criteria = question.get("criteria")
    _require(isinstance(criteria, dict) and len(criteria) > 0,
             f"request.questions[{key!r}].criteria must be a non-empty object")
    for label, description in criteria.items():
        _require(_is_entry_type(description),
                 f"request.questions[{key!r}].criteria[{label!r}] must be an EntryType value")


def validate_result(raw: Any) -> dict:
    result = parse_json(raw, "result")
    _require(isinstance(result, dict), "result must be a JSON object")
    model = result.get("model")
    _require(isinstance(model, str) and model != "", "result.model must be a non-empty string")
    _require(isinstance(result.get("answers"), dict), "result.answers must be an object")
    _validate_usage(result.get("usage"))
    return result


def _validate_usage(usage: Any) -> None:
    _require(isinstance(usage, dict), "result.usage must be an object")
    for field in ("input_tokens", "output_tokens"):
        _require(_is_nonnegative_int(usage.get(field)),
                 f"result.usage.{field} must be a non-negative integer")


def _validate_noul_answer(key: str, answer: dict) -> None:
    _require(_is_unit_interval(answer.get("noul")),
             f"answers[{key!r}].noul must be a finite number in [0, 1]")


def _validate_choice_answer(key: str, answer: dict, criteria: dict) -> None:
    _require(answer.get("choice") in criteria,
             f"answers[{key!r}].choice must be one of the request criteria")
    _require(_is_unit_interval(answer.get("confidence")),
             f"answers[{key!r}].confidence must be a finite number in [0, 1]")
    probabilities = answer.get("probabilities")
    _require(isinstance(probabilities, dict), f"answers[{key!r}].probabilities must be an object")
    _require(set(probabilities.keys()) == set(criteria.keys()),
             f"answers[{key!r}].probabilities keys must exactly match the request criteria keys")
    for label, probability in probabilities.items():
        _require(_is_unit_interval(probability),
                 f"answers[{key!r}].probabilities[{label!r}] must be a finite number in [0, 1]")


def validate_pair(request: dict, result: dict) -> None:
    """Cross-check a validated request against a validated result's answers."""
    questions = request["questions"]
    answers = result["answers"]
    for key, question in questions.items():
        _require(key in answers, f"result.answers is missing key {key!r}")
        answer = answers[key]
        _require(isinstance(answer, dict), f"answers[{key!r}] must be an object")
        question_type = question["type"]
        answer_type = answer.get("type")
        _require(answer_type == question_type,
                 f"answers[{key!r}].type {answer_type!r} does not match "
                 f"request question type {question_type!r}")
        if question_type == "noul":
            _validate_noul_answer(key, answer)
        else:
            _validate_choice_answer(key, answer, question["criteria"])


def check(request_raw: Any, result_raw: Any) -> None:
    """Validate a request/result pair. Raises ContractError on any violation."""
    request = validate_request(request_raw)
    result = validate_result(result_raw)
    validate_pair(request, result)


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(f"usage: {argv[0]} <request.json> <result.json>", file=sys.stderr)
        return EXIT_USAGE
    request_path, result_path = Path(argv[1]), Path(argv[2])
    try:
        request_raw = request_path.read_text(encoding="utf-8")
        result_raw = result_path.read_text(encoding="utf-8")
    except OSError as exc:
        print(f"cannot read input: {exc}", file=sys.stderr)
        return EXIT_USAGE
    try:
        check(request_raw, result_raw)
    except ContractError as exc:
        print(f"contract violation: {exc}", file=sys.stderr)
        return EXIT_VIOLATION
    print("ok")
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main(sys.argv))
