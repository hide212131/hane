"""Tests for aadw_jev_adoption_gate.py: fixture-only, no network, no side effects.

These tests check only the current-context freshness / adoption judgment for
an already-produced Jev request/result pair. They never call the Jev API or
GitHub, and a pass here is not evidence of a real service connection or of
any routing/merge decision, which this module never makes.
"""

from __future__ import annotations

import copy
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import aadw_jev_adoption_gate as gate

HEAD_A = "a" * 40
HEAD_B = "b" * 40
BASE_A = "c" * 40
BASE_B = "d" * 40

ACTION_LABELS = ["continue", "request_fix", "complete"]


def valid_jev_request() -> dict:
    return {
        "state": {"pr": 352},
        "questions": {
            "action": {
                "type": "choice",
                "criteria": {label: f"{label} description" for label in ACTION_LABELS},
            }
        },
        "model": "system-one-default",
    }


def valid_jev_result(choice: str = "continue") -> dict:
    return {
        "model": "system-one-default",
        "answers": {
            "action": {
                "type": "choice",
                "choice": choice,
                "confidence": 0.9,
                "probabilities": {label: 1.0 if label == choice else 0.0 for label in ACTION_LABELS},
            }
        },
        "usage": {"input_tokens": 10, "output_tokens": 5},
    }


def valid_context() -> dict:
    return {
        "repository": "hide212131/hane",
        "pr_number": 352,
        "expected_head_sha": HEAD_A,
        "current_head_sha": HEAD_A,
        "base_sensitive": False,
        "expected_base_sha": None,
        "current_base_sha": None,
        "acceptance_evidence_id": "evidence-1",
        "ci_status": "success",
        "review_status": "success",
        "gui_required": False,
        "gui_status": None,
        "jev_status": "success",
        "jev_request": valid_jev_request(),
        "jev_result": valid_jev_result(),
        "action_question_key": "action",
        "requested_action_labels": list(ACTION_LABELS),
        "current_allowed_action_labels": list(ACTION_LABELS),
    }


def base_sensitive_context() -> dict:
    context = valid_context()
    context["base_sensitive"] = True
    context["expected_base_sha"] = BASE_A
    context["current_base_sha"] = BASE_A
    return context


class AdoptableTests(unittest.TestCase):
    def test_exact_head_and_evidence_is_adoptable(self):
        decision = gate.evaluate_adoption(valid_context())
        self.assertTrue(decision.adoptable)

    def test_base_sensitive_with_matching_base_is_adoptable(self):
        decision = gate.evaluate_adoption(base_sensitive_context())
        self.assertTrue(decision.adoptable)

    def test_base_independent_with_moved_base_is_still_adoptable(self):
        context = valid_context()
        context["base_sensitive"] = False
        context["expected_base_sha"] = BASE_A
        context["current_base_sha"] = BASE_B
        decision = gate.evaluate_adoption(context)
        self.assertTrue(decision.adoptable)

    def test_gui_not_required_without_gui_status_is_adoptable(self):
        context = valid_context()
        context["gui_required"] = False
        context["gui_status"] = None
        decision = gate.evaluate_adoption(context)
        self.assertTrue(decision.adoptable)

    def test_gui_required_with_success_is_adoptable(self):
        context = valid_context()
        context["gui_required"] = True
        context["gui_status"] = "success"
        decision = gate.evaluate_adoption(context)
        self.assertTrue(decision.adoptable)


class HeadFreshnessTests(unittest.TestCase):
    def test_moved_head_is_blocked(self):
        context = valid_context()
        context["current_head_sha"] = HEAD_B
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_missing_current_head_is_blocked(self):
        context = valid_context()
        del context["current_head_sha"]
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_invalid_head_sha_shape_is_blocked(self):
        for value in ("not-a-sha", "a" * 39, "a" * 41, HEAD_A.upper()[:39] + "!", 12345, None):
            with self.subTest(value=value):
                context = valid_context()
                context["current_head_sha"] = value
                decision = gate.evaluate_adoption(context)
                self.assertFalse(decision.adoptable)

    def test_uppercase_hex_head_matches_lowercase(self):
        context = valid_context()
        context["expected_head_sha"] = HEAD_A.upper()
        context["current_head_sha"] = HEAD_A
        decision = gate.evaluate_adoption(context)
        self.assertTrue(decision.adoptable)


class BaseFreshnessTests(unittest.TestCase):
    def test_base_sensitive_moved_base_is_blocked(self):
        context = base_sensitive_context()
        context["current_base_sha"] = BASE_B
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_base_sensitive_missing_base_sha_is_blocked(self):
        context = base_sensitive_context()
        del context["expected_base_sha"]
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_base_sensitive_invalid_base_sha_is_blocked(self):
        context = base_sensitive_context()
        context["current_base_sha"] = "not-a-sha"
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_missing_base_sensitive_flag_is_blocked(self):
        context = valid_context()
        del context["base_sensitive"]
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)


class EvidenceAndCiReviewTests(unittest.TestCase):
    def test_empty_acceptance_evidence_id_is_blocked(self):
        context = valid_context()
        context["acceptance_evidence_id"] = ""
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_whitespace_only_acceptance_evidence_id_is_blocked(self):
        context = valid_context()
        context["acceptance_evidence_id"] = "   "
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_ci_failure_or_blocked_or_unknown_is_blocked(self):
        for status in ("failure", "blocked", "unknown", "missing", "", None):
            with self.subTest(status=status):
                context = valid_context()
                context["ci_status"] = status
                decision = gate.evaluate_adoption(context)
                self.assertFalse(decision.adoptable)

    def test_review_failure_or_blocked_is_blocked(self):
        for status in ("failure", "blocked", "unknown", "missing"):
            with self.subTest(status=status):
                context = valid_context()
                context["review_status"] = status
                decision = gate.evaluate_adoption(context)
                self.assertFalse(decision.adoptable)


class GuiFreshnessTests(unittest.TestCase):
    def test_gui_required_unknown_is_blocked(self):
        context = valid_context()
        context["gui_required"] = True
        context["gui_status"] = "unknown"
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_gui_required_failed_or_blocked_is_blocked(self):
        for status in ("failure", "blocked", "unknown", "missing", None):
            with self.subTest(status=status):
                context = valid_context()
                context["gui_required"] = True
                context["gui_status"] = status
                decision = gate.evaluate_adoption(context)
                self.assertFalse(decision.adoptable)

    def test_gui_required_missing_flag_is_blocked(self):
        context = valid_context()
        del context["gui_required"]
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)


class JevContractDelegationTests(unittest.TestCase):
    def test_malformed_jev_request_is_blocked(self):
        context = valid_context()
        context["jev_request"]["questions"] = {}
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_missing_answer_is_blocked(self):
        context = valid_context()
        del context["jev_result"]["answers"]["action"]
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_non_finite_probability_is_blocked(self):
        context = valid_context()
        context["jev_result"]["answers"]["action"]["confidence"] = float("nan")
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_choice_outside_request_criteria_is_blocked(self):
        context = valid_context()
        context["jev_result"]["answers"]["action"]["choice"] = "unknown-label"
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)


class JevStatusTests(unittest.TestCase):
    def test_jev_failure_is_blocked_not_converted_to_fallback(self):
        context = valid_context()
        context["jev_status"] = "failure"
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)
        lowered = decision.reason.lower()
        self.assertNotIn("codex", lowered)
        self.assertNotIn("complete", lowered)
        self.assertNotIn("merge", lowered)

    def test_jev_unavailable_is_blocked(self):
        context = valid_context()
        context["jev_status"] = "unavailable"
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_missing_jev_status_is_blocked(self):
        context = valid_context()
        del context["jev_status"]
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)


class ActionLabelTests(unittest.TestCase):
    def test_empty_allowed_action_labels_is_blocked(self):
        context = valid_context()
        context["current_allowed_action_labels"] = []
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_missing_allowed_action_labels_is_blocked(self):
        context = valid_context()
        del context["current_allowed_action_labels"]
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_choice_outside_current_allowed_labels_is_blocked(self):
        context = valid_context()
        context["jev_request"]["questions"]["action"]["criteria"]["extra"] = "extra description"
        context["jev_result"] = valid_jev_result("extra")
        context["jev_result"]["answers"]["action"]["probabilities"]["extra"] = 1.0
        # keep contract-level criteria matching (extra is a valid request
        # criterion) but do not add it to the currently allowed set.
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_current_candidate_set_changed_is_blocked(self):
        context = valid_context()
        context["current_allowed_action_labels"] = ACTION_LABELS + ["new_label"]
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_requested_candidate_set_changed_is_blocked(self):
        context = valid_context()
        context["requested_action_labels"] = ACTION_LABELS[:-1]
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_requested_action_labels_not_matching_actual_request_criteria_is_blocked(self):
        # The actual jev_request criteria has an extra candidate ("obsolete")
        # that the context's requested/current declarations both omit, and
        # Jev selected a label that is in the (understated) current allowed
        # set. This must still be blocked: requested_action_labels must be
        # verified against the real request criteria, not merely against
        # current_allowed_action_labels.
        context = valid_context()
        context["jev_request"]["questions"]["action"]["criteria"]["obsolete"] = "obsolete description"
        context["jev_result"]["answers"]["action"]["probabilities"]["obsolete"] = 0.0
        # requested_action_labels and current_allowed_action_labels both
        # under-report the real candidate set and still agree with each other.
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_action_question_key_not_a_choice_question_is_blocked(self):
        context = valid_context()
        context["jev_request"]["questions"]["action"] = {"type": "noul"}
        context["jev_result"]["answers"]["action"] = {"type": "noul", "noul": 0.5}
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_action_question_key_missing_from_request_is_blocked(self):
        context = valid_context()
        context["action_question_key"] = "does_not_exist"
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_whitespace_only_action_question_key_is_blocked(self):
        context = valid_context()
        context["action_question_key"] = "   "
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)


class ContextShapeTests(unittest.TestCase):
    def test_non_dict_context_is_blocked(self):
        decision = gate.evaluate_adoption(["not", "a", "dict"])
        self.assertFalse(decision.adoptable)

    def test_missing_repository_is_blocked(self):
        context = valid_context()
        del context["repository"]
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_whitespace_only_repository_is_blocked(self):
        context = valid_context()
        context["repository"] = "   "
        decision = gate.evaluate_adoption(context)
        self.assertFalse(decision.adoptable)

    def test_non_positive_pr_number_is_blocked(self):
        for value in (0, -1, True, "352", None):
            with self.subTest(value=value):
                context = valid_context()
                context["pr_number"] = value
                decision = gate.evaluate_adoption(context)
                self.assertFalse(decision.adoptable)

    def test_evaluate_adoption_never_raises_on_arbitrary_junk(self):
        for junk in (None, 42, "text", [], {}, {"repository": 1}):
            with self.subTest(junk=junk):
                decision = gate.evaluate_adoption(junk)  # must not raise
                self.assertFalse(decision.adoptable)

    def test_deep_copy_of_valid_context_is_still_adoptable(self):
        # Sanity check that valid_context() has no shared-mutable-state
        # surprises across tests.
        context = copy.deepcopy(valid_context())
        decision = gate.evaluate_adoption(context)
        self.assertTrue(decision.adoptable)


if __name__ == "__main__":
    unittest.main()
