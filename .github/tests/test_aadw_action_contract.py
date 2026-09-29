import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("contract", ROOT / "scripts/aadw_action_contract.py")
contract = importlib.util.module_from_spec(spec)
spec.loader.exec_module(contract)
HEAD = "a" * 40


def request(action="implement", category="product", head=HEAD, evidence="https://github.com/o/r/actions/runs/1"):
    return (f"{contract.MARKERS[action]}\nAADW_ACTION: {action}\nAADW_TARGET_HEAD: {head}\n"
            f"AADW_FAILURE_CLASS: {category}\nAADW_ROOT_CAUSE: wheel-timing\nAADW_EVIDENCE: {evidence}\n")


def pr():
    return {"number": 1, "state": "open", "body": "", "head": {"sha": HEAD, "repo": {"full_name": "o/r"}}}


class RequestTests(unittest.TestCase):
    def validate(self, body=None, target=None, history=None):
        return contract.validate_request(body or request(), target or pr(), "o/r", history or [], "owner", 100)

    def test_current_product_request(self):
        self.assertEqual(self.validate()["action"], "implement")

    def test_unknown_measurement_environment_cannot_trigger_fix(self):
        for category in ("measurement", "environment", "unknown"):
            with self.subTest(category=category), self.assertRaises(ValueError):
                self.validate(request(category=category))

    def test_diagnosis_accepts_uncertainty(self):
        self.assertEqual(self.validate(request("diagnose", "unknown"))["action"], "diagnose")

    def test_missing_duplicate_or_mixed_fields_fail_closed(self):
        for body in (request().replace("AADW_ACTION: implement\n", ""),
                     request() + "AADW_ACTION: implement\n",
                     request() + "AADW_DIAGNOSTIC_REQUEST_V1\n"):
            with self.subTest(body=body), self.assertRaises(ValueError):
                self.validate(body)

    def test_closed_paused_foreign_and_stale_rejected(self):
        targets = []
        p = pr(); p["state"] = "closed"; targets.append(p)
        p = pr(); p["body"] = "<!-- AADW_PAUSED -->"; targets.append(p)
        p = pr(); p["labels"] = [{"name": "aadw:paused"}]; targets.append(p)
        p = pr(); p["head"]["sha"] = "b" * 40; targets.append(p)
        p = pr(); p["head"]["repo"]["full_name"] = "other/fork"; targets.append(p)
        for target in targets:
            with self.subTest(target=target), self.assertRaises(ValueError):
                self.validate(target=target)

    def test_same_head_resend_rejected(self):
        with self.assertRaises(ValueError):
            self.validate(history=[{"id": 1, "user": {"login": "owner"}, "body": request()}])

    def test_two_old_heads_with_same_evidence_require_diagnosis(self):
        history = [{"id": i, "user": {"login": "owner"}, "body": request(head=c * 40)}
                   for i, c in enumerate("bc", 1)]
        with self.assertRaises(ValueError):
            self.validate(history=history)
        self.validate(request(evidence="https://github.com/o/r/actions/runs/2"), history=history)
        self.validate(request("diagnose", "unknown"), history=history)

    def test_other_users_and_current_comment_do_not_count(self):
        self.validate(history=[{"id": 1, "user": {"login": "outsider"}, "body": request()},
                               {"id": 100, "user": {"login": "owner"}, "body": request()}])


class ResultTests(unittest.TestCase):
    def report(self):
        return {"schema_version": 1, "target_sha": HEAD, "failure_class": "measurement",
                "summary": "製品変更は不要。", "evidence": "取得時間を確認した。",
                "next_observation": "同じ時計で入力受信を確認する。"}

    def test_no_patch_diagnosis_is_success_not_product_pass(self):
        result = contract.diagnostic_result(self.report(), contract.parse_request(request("diagnose", "measurement")), False)
        self.assertEqual(result["result"], "diagnosis_completed")
        self.assertEqual(result["product_acceptance"], "not_evaluated")

    def test_diagnostic_patch_is_rejected(self):
        with self.assertRaises(ValueError):
            contract.diagnostic_result(self.report(), contract.parse_request(request("diagnose", "measurement")), True)

    def test_implementation_cannot_be_reported_as_diagnostic_success(self):
        with self.assertRaises(ValueError):
            contract.diagnostic_result(self.report(), contract.parse_request(request()), False)

    def test_wrong_head_missing_reason_or_invalid_category_rejected(self):
        for key, value in (("target_sha", "b" * 40), ("summary", ""), ("failure_class", "initial"), ("evidence", 10)):
            report = self.report(); report[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                contract.diagnostic_result(report, contract.parse_request(request("diagnose", "measurement")), False)

    def test_missing_or_symlink_result_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "result.json"
            with self.assertRaises(ValueError): contract.read_json(path)
            original = Path(temp) / "original.json"; original.write_text("{}")
            path.symlink_to(original)
            with self.assertRaises(ValueError): contract.read_json(path)


if __name__ == "__main__":
    unittest.main()
