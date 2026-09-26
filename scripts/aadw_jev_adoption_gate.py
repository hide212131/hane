#!/usr/bin/env python3
"""Current-context freshness / adoption gate for a Jev bounded-choice decision.

This answers a single narrow question for one already-produced Jev
request/result pair (validated in shape by aadw_jev_contract.py): given the
*current* PR facts observed by the Commander right now, is that decision still
safe to adopt, or is it blocked?

This module does not call the Jev API, the GitHub API, or any network/service,
and it holds no persistent state. It performs no repository mutation and picks
no next action: it returns only an `adoptable` boolean and a short `reason`.
In particular, a `blocked` result (stale head/base, missing/failed CI or
review, a required GUI scenario that is not a success, a Jev failure, or a
candidate action label outside the current allowed set) must never be turned
into a Codex fallback, a COMPLETE signal, or merge permission by any caller;
those decisions remain the Commander's alone.

Expected `context` shape (all keys required unless noted):
    {
      "repository": str,                       # non-empty
      "pr_number": int,                         # positive, not bool
      "expected_head_sha": str,                 # 40 hex chars
      "current_head_sha": str,                  # 40 hex chars
      "base_sensitive": bool,
      "expected_base_sha": str | None,          # 40 hex chars, required iff base_sensitive
      "current_base_sha": str | None,           # 40 hex chars, required iff base_sensitive
      "acceptance_evidence_id": str,            # non-empty
      "ci_status": str,                         # only "success" is adoptable
      "review_status": str,                     # only "success" is adoptable
      "gui_required": bool,
      "gui_status": str | None,                 # required to be "success" iff gui_required
      "jev_status": str,                        # only "success" is adoptable
      "jev_request": dict | str,                # per aadw_jev_contract request shape
      "jev_result": dict | str,                 # per aadw_jev_contract result shape
      "action_question_key": str,               # non-empty key into questions/answers
      "requested_action_labels": [str, ...],    # non-empty, candidate set when Jev was asked
      "current_allowed_action_labels": [str, ...],  # non-empty, candidate set observed now
    }
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

_SCRIPTS_DIR = str(Path(__file__).resolve().parent)
if _SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, _SCRIPTS_DIR)

from aadw_jev_contract import ContractError, validate_pair, validate_request, validate_result

_SHA_RE = re.compile(r"^[0-9a-fA-F]{40}$")
_STATUS_SUCCESS = "success"


class AdoptionBlocked(ValueError):
    """The current context does not support adopting this Jev decision."""


@dataclass(frozen=True)
class AdoptionDecision:
    """A small, side-effect-free adopt/block judgment with its reason."""

    adoptable: bool
    reason: str


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise AdoptionBlocked(message)


def _require_sha(value: Any, label: str) -> str:
    _require(isinstance(value, str) and _SHA_RE.match(value) is not None,
              f"{label} must be a 40-character hex commit SHA")
    return value.lower()


def _require_non_empty_str(value: Any, label: str) -> str:
    _require(isinstance(value, str) and value.strip() != "",
              f"{label} must be a non-empty string")
    return value


def _require_bool(value: Any, label: str) -> bool:
    _require(isinstance(value, bool), f"{label} must be a boolean")
    return value


def _require_label_list(value: Any, label: str) -> list[str]:
    _require(isinstance(value, list) and len(value) > 0, f"{label} must be a non-empty list")
    for item in value:
        _require(isinstance(item, str) and item != "",
                  f"{label} entries must be non-empty strings")
    return value


def _require_status_success(value: Any, label: str) -> None:
    _require(isinstance(value, str) and value != "",
              f"{label} must be a non-empty string status")
    _require(value == _STATUS_SUCCESS,
              f"{label} must be {_STATUS_SUCCESS!r} to adopt this decision (got {value!r})")


def _check_head(context: dict) -> None:
    expected_head = _require_sha(context.get("expected_head_sha"), "expected_head_sha")
    current_head = _require_sha(context.get("current_head_sha"), "current_head_sha")
    _require(expected_head == current_head,
              "current_head_sha does not match expected_head_sha; head has moved")


def _check_base(context: dict) -> None:
    base_sensitive = _require_bool(context.get("base_sensitive"), "base_sensitive")
    if not base_sensitive:
        return
    expected_base = _require_sha(context.get("expected_base_sha"), "expected_base_sha")
    current_base = _require_sha(context.get("current_base_sha"), "current_base_sha")
    _require(expected_base == current_base,
              "current_base_sha does not match expected_base_sha for a base-sensitive decision")


def _check_gui(context: dict) -> None:
    gui_required = _require_bool(context.get("gui_required"), "gui_required")
    if not gui_required:
        return
    gui_status = context.get("gui_status")
    _require_status_success(gui_status, "gui_status")


def _check_action_labels(context: dict, request: dict, result: dict) -> None:
    action_key = _require_non_empty_str(context.get("action_question_key"), "action_question_key")
    question = request["questions"].get(action_key)
    _require(question is not None,
              f"action_question_key {action_key!r} is not a question in jev_request")
    _require(question.get("type") == "choice",
              f"action_question_key {action_key!r} must be a 'choice' question")
    # validate_pair (already run by the caller) guarantees a matching answer
    # exists for every request question key, with a matching answer type.
    answer = result["answers"][action_key]
    selected_label = answer["choice"]

    request_criteria_labels = set(question["criteria"].keys())

    requested_labels = _require_label_list(context.get("requested_action_labels"),
                                            "requested_action_labels")
    current_labels = _require_label_list(context.get("current_allowed_action_labels"),
                                          "current_allowed_action_labels")
    _require(set(requested_labels) == request_criteria_labels,
              "requested_action_labels does not match the actual jev_request criteria "
              "labels for this Jev decision")
    _require(request_criteria_labels == set(current_labels),
              "current_allowed_action_labels no longer matches the candidate set "
              "recorded when this Jev decision was requested")
    _require(selected_label in current_labels,
              f"selected choice {selected_label!r} is outside the current allowed action labels")


def evaluate_adoption(context: Any) -> AdoptionDecision:
    """Decide whether a Jev choice decision is adoptable under current facts.

    Never raises: any violation, including a malformed context or a Jev
    contract violation, is returned as `AdoptionDecision(False, reason)`.
    """
    try:
        _require(isinstance(context, dict), "context must be an object")
        _require_non_empty_str(context.get("repository"), "repository")
        pr_number = context.get("pr_number")
        _require(isinstance(pr_number, int) and not isinstance(pr_number, bool) and pr_number > 0,
                  "pr_number must be a positive integer")

        _check_head(context)
        _check_base(context)

        _require_non_empty_str(context.get("acceptance_evidence_id"), "acceptance_evidence_id")
        _require_status_success(context.get("ci_status"), "ci_status")
        _require_status_success(context.get("review_status"), "review_status")
        _check_gui(context)

        # A Jev failure/unavailable/non-success status is blocked outright; it
        # must never be reinterpreted as a Codex fallback or a COMPLETE signal
        # by this gate or its caller.
        _require_status_success(context.get("jev_status"), "jev_status")

        try:
            request = validate_request(context.get("jev_request"))
            result = validate_result(context.get("jev_result"))
            validate_pair(request, result)
        except ContractError as exc:
            raise AdoptionBlocked(f"jev contract violation: {exc}") from exc

        _check_action_labels(context, request, result)
    except AdoptionBlocked as exc:
        return AdoptionDecision(False, str(exc))

    return AdoptionDecision(True, "current head/base/evidence and Jev choice decision are fresh")
