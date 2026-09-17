#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

MODULE_PATH = Path(__file__).resolve().parents[1] / "scripts" / "aadw_gui_command.py"
SPEC = importlib.util.spec_from_file_location("aadw_gui_command", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
command = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(command)

SHA = "a" * 40


def pull_request(**overrides):
    value = {
        "state": "open",
        "draft": False,
        "head": {
            "sha": SHA,
            "ref": "feature/gui-command",
            "repo": {"full_name": "owner/repo"},
        },
        "base": {"ref": "main"},
    }
    value.update(overrides)
    return value


class ParseCommandTests(unittest.TestCase):
    def test_accepts_exact_head_command(self):
        self.assertEqual(command.parse_command("/gui-validate head"), "head")

    def test_accepts_exact_merge_command(self):
        self.assertEqual(command.parse_command("/gui-validate merge"), "merge")

    def test_accepts_outer_whitespace_but_not_embedded_prose(self):
        self.assertEqual(command.parse_command(" \r\n/gui-validate merge\r\n "), "merge")
        self.assertIsNone(command.parse_command("このPRは /gui-validate merge した方がよい"))

    def test_rejects_invalid_command_forms(self):
        invalid = (
            "/gui-validate",
            "/gui-validate foo",
            "/gui validate merge",
            "foo /gui-validate merge",
            "/gui-validate merge extra",
            "/gui-validate\nmerge",
        )
        for body in invalid:
            with self.subTest(body=body):
                self.assertIsNone(command.parse_command(body))


class TrustedProcedureTests(unittest.TestCase):
    def _procedure(self, source: str):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "procedure.py"
            path.write_text(source, encoding="utf-8")
            return command.trusted_procedure(path)

    def test_reads_single_procedure_version(self):
        self.assertEqual(
            self._procedure('PROCEDURE_VERSION = "hosted-gui-interaction/7"\n'),
            "hosted-gui-interaction/7",
        )

    def test_rejects_missing_or_duplicate_procedure_version(self):
        for source in (
            "OTHER = 'value'\n",
            "PROCEDURE_VERSION = 'a'\nPROCEDURE_VERSION = 'b'\n",
            "PROCEDURE_VERSION = 7\n",
        ):
            with self.subTest(source=source):
                with self.assertRaises(command.RequestError):
                    self._procedure(source)


class BuildRequestTests(unittest.TestCase):
    def build(self, **overrides):
        values = {
            "repository": "owner/repo",
            "pr_number": 123,
            "actor": "trusted-user",
            "permission": "write",
            "pr": pull_request(),
            "execution_context": "merge",
            "procedure": "hosted-gui-interaction/7",
            "comment_id": 456,
        }
        values.update(overrides)
        return command.build_request(**values)

    def test_builds_request_from_current_pr_facts(self):
        request = self.build()
        self.assertEqual(request["pr_number"], "123")
        self.assertEqual(request["target_head_sha"], SHA)
        self.assertEqual(request["execution_context"], "merge")
        self.assertEqual(request["procedure"], "hosted-gui-interaction/7")
        self.assertEqual(request["request_comment_id"], "456")
        self.assertEqual(request["request_actor"], "trusted-user")
        self.assertEqual(
            request["request_identity"],
            f"pr=123;head={SHA};context=merge;procedure=hosted-gui-interaction/7",
        )
        self.assertEqual(
            request["run_name"],
            f"AADW GUI PR #123 @ {SHA} merge hosted-gui-interaction/7",
        )

    def test_accepts_write_maintain_and_admin_only(self):
        for permission in ("write", "maintain", "admin"):
            with self.subTest(permission=permission):
                self.assertEqual(self.build(permission=permission)["target_head_sha"], SHA)
        for permission in ("read", "triage", "", "none"):
            with self.subTest(permission=permission):
                with self.assertRaises(command.RequestError):
                    self.build(permission=permission)

    def test_rejects_closed_or_draft_pull_request(self):
        with self.assertRaises(command.RequestError):
            self.build(pr=pull_request(state="closed"))
        with self.assertRaises(command.RequestError):
            self.build(pr=pull_request(draft=True))

    def test_rejects_fork_pull_request(self):
        pr = pull_request()
        pr["head"]["repo"]["full_name"] = "outside/fork"
        with self.assertRaises(command.RequestError):
            self.build(pr=pr)

    def test_rejects_invalid_current_head_or_refs(self):
        pr = pull_request()
        pr["head"]["sha"] = "A" * 40
        with self.assertRaises(command.RequestError):
            self.build(pr=pr)

        pr = pull_request()
        pr["head"]["ref"] = ""
        with self.assertRaises(command.RequestError):
            self.build(pr=pr)

        pr = pull_request()
        pr["base"]["ref"] = ""
        with self.assertRaises(command.RequestError):
            self.build(pr=pr)

    def test_rejects_unsupported_execution_context(self):
        with self.assertRaises(command.RequestError):
            self.build(execution_context="latest")


class DuplicateTests(unittest.TestCase):
    RUN_NAME = f"AADW GUI PR #123 @ {SHA} merge hosted-gui-interaction/7"

    def test_queued_or_in_progress_run_is_duplicate(self):
        for status in ("queued", "in_progress", "pending"):
            with self.subTest(status=status):
                run = {"id": 10, "display_title": self.RUN_NAME, "status": status, "conclusion": None}
                self.assertEqual(command.find_duplicate([run], self.RUN_NAME), run)

    def test_successful_run_is_duplicate(self):
        run = {"id": 10, "display_title": self.RUN_NAME, "status": "completed", "conclusion": "success"}
        self.assertEqual(command.find_duplicate([run], self.RUN_NAME), run)

    def test_failed_run_can_be_requested_again(self):
        run = {"id": 10, "display_title": self.RUN_NAME, "status": "completed", "conclusion": "failure"}
        self.assertIsNone(command.find_duplicate([run], self.RUN_NAME))

    def test_different_request_identity_is_not_duplicate(self):
        run = {"id": 10, "display_title": self.RUN_NAME + "-different", "status": "queued", "conclusion": None}
        self.assertIsNone(command.find_duplicate([run], self.RUN_NAME))

    def test_older_success_still_prevents_duplicate_after_failed_retry(self):
        runs = [
            {"id": 11, "display_title": self.RUN_NAME, "status": "completed", "conclusion": "failure"},
            {"id": 10, "display_title": self.RUN_NAME, "status": "completed", "conclusion": "success"},
        ]
        self.assertEqual(command.find_duplicate(runs, self.RUN_NAME)["id"], 10)


if __name__ == "__main__":
    unittest.main()
