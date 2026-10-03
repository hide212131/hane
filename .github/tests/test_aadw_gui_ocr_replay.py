#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = REPOSITORY_ROOT / "scripts" / "hosted_gui_ocr_replay.py"
SPEC = importlib.util.spec_from_file_location("hosted_gui_ocr_replay", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
replay = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(replay)

REPOSITORY = "hide212131/hane"
HEAD_SHA = "8b09d745c0422126739890af9d57a742704fc652"
BASE_SHA = "71d8e2cdbb52ee9b7c75e628f39b811afe46e002"
CONTROL_SHA = BASE_SHA
MERGE_SHA = "658fcccb4af88b7b0a0d811591111b84772131b2"
RUN_ID = 37111994530
ARTIFACT_ID = 11269844171
ATTEMPT = 1


def source_records():
    run = {
        "id": RUN_ID,
        "status": "completed",
        "event": "workflow_dispatch",
        "path": ".github/workflows/aadw-gui-validation.yml@refs/heads/main",
        "head_branch": "main",
        "head_repository": {"full_name": REPOSITORY},
        "head_sha": CONTROL_SHA,
        "run_attempt": ATTEMPT,
    }
    artifact = {
        "id": ARTIFACT_ID,
        "name": f"aadw-gui-evidence-pr-395-{RUN_ID}-{ATTEMPT}",
        "expired": False,
        "workflow_run": {"id": RUN_ID},
    }
    return run, artifact


def source_context():
    return {
        "schema_version": 1,
        "repository": REPOSITORY,
        "pr_number": 395,
        "requested_head_sha": HEAD_SHA,
        "requested_base_sha": BASE_SHA,
        "observed_base_sha": BASE_SHA,
        "execution_context": "merge",
        "execution_sha": MERGE_SHA,
        "control_sha": CONTROL_SHA,
        "procedure": "hosted-scroll-inertia/12",
        "workflow_run_id": str(RUN_ID),
        "workflow_run_attempt": str(ATTEMPT),
    }


def source_result():
    return {
        "overall_result": "blocked",
        "overall_reason": "文書先頭への移動後に先頭行を確認できない",
        "observation_quality": {
            "diagnosis_required": True,
            "steps": [
                {"name": name, "failure_class": "measurement", "product_cause_proven": False}
                for name in ("lines_coast", "direction_reversal", "pixels_direct_follow")
            ],
        },
        "scenarios": [{
            "name": "scroll_inertia",
            "result": "blocked",
            "steps": [{"name": "scroll_direction_calibration", "result": "blocked"}],
        }],
    }


def provenance():
    run, artifact = source_records()
    return replay.validate_source_metadata(
        run,
        artifact,
        source_run_id=RUN_ID,
        artifact_id=ARTIFACT_ID,
        pr_number=395,
        repository=REPOSITORY,
        default_branch="main",
    )


class VisibleLinesTests(unittest.TestCase):
    def test_extracts_unique_line_numbers_case_insensitively(self):
        self.assertEqual(replay.visible_lines("LINE 001\nline 007\nLINE 0010\nLINE 007"), [1, 7, 10])

    def test_ignores_unrelated_numbers(self):
        self.assertEqual(replay.visible_lines("revision 4500\nframe 21"), [])


class SourceMetadataTests(unittest.TestCase):
    def test_accepts_the_exact_completed_default_branch_run_and_artifact(self):
        result = provenance()
        self.assertEqual(result["source_workflow_sha"], CONTROL_SHA)
        self.assertEqual(result["source_artifact_id"], ARTIFACT_ID)
        self.assertEqual(result["source_artifact_name"], f"aadw-gui-evidence-pr-395-{RUN_ID}-1")

    def test_rejects_an_artifact_from_another_run(self):
        run, artifact = source_records()
        artifact["workflow_run"]["id"] += 1
        with self.assertRaises(replay.ReplayError):
            replay.validate_source_metadata(
                run, artifact, source_run_id=RUN_ID, artifact_id=ARTIFACT_ID,
                pr_number=395, repository=REPOSITORY, default_branch="main",
            )

    def test_rejects_a_non_default_or_non_gui_workflow_run(self):
        for field, value in (("head_branch", "feature"), ("event", "pull_request"), ("status", "in_progress")):
            run, artifact = source_records()
            run[field] = value
            with self.subTest(field=field), self.assertRaises(replay.ReplayError):
                replay.validate_source_metadata(
                    run, artifact, source_run_id=RUN_ID, artifact_id=ARTIFACT_ID,
                    pr_number=395, repository=REPOSITORY, default_branch="main",
                )


class SourceContextTests(unittest.TestCase):
    def validate(self, context=None, result=None):
        replay.validate_source_context(
            context or source_context(), result or source_result(), provenance(),
            expected_pr_number=395, expected_repository=REPOSITORY, expected_head_sha=HEAD_SHA,
        )

    def test_binds_exact_head_and_blocked_direction_calibration(self):
        self.validate()

    def test_rejects_different_candidate_head(self):
        value = source_context()
        value["requested_head_sha"] = "a" * 40
        with self.assertRaises(replay.ReplayError):
            self.validate(context=value)

    def test_rejects_artifact_that_was_not_blocked_at_calibration(self):
        value = source_result()
        value["scenarios"][0]["steps"][0]["result"] = "pass"
        with self.assertRaises(replay.ReplayError):
            self.validate(result=value)

    def test_rejects_source_without_measurement_diagnosis(self):
        value = source_result()
        value["observation_quality"]["diagnosis_required"] = False
        with self.assertRaises(replay.ReplayError):
            self.validate(result=value)

    def test_rejects_a_source_step_classified_as_product_failure(self):
        value = source_result()
        value["observation_quality"]["steps"][0]["failure_class"] = "product"
        value["observation_quality"]["steps"][0]["product_cause_proven"] = True
        with self.assertRaises(replay.ReplayError):
            self.validate(result=value)

class SourceImageTests(unittest.TestCase):
    def test_resolves_only_the_expected_image_under_the_artifact_root(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            image = root / replay.SOURCE_IMAGE
            image.parent.mkdir(parents=True)
            image.write_bytes(b"png")
            self.assertEqual(replay.resolve_source_image(root), image.resolve())

    def test_rejects_missing_image(self):
        with tempfile.TemporaryDirectory() as temp, self.assertRaises(FileNotFoundError):
            replay.resolve_source_image(Path(temp))


class OcrHelperLaunchFailureTests(unittest.TestCase):
    def test_records_helper_oserror_in_report_and_fails_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            image = root / replay.SOURCE_IMAGE
            image.parent.mkdir(parents=True)
            image.write_bytes(b"png")
            (root / "aadw-context.json").write_text(
                json.dumps(source_context()), encoding="utf-8",
            )
            (root / "result.json").write_text(
                json.dumps(source_result()), encoding="utf-8",
            )
            provenance_path = root / "provenance.json"
            provenance_path.write_text(json.dumps(provenance()), encoding="utf-8")
            report_path = root / "ocr-replay.json"
            helper_sha = "d" * 40
            args = SimpleNamespace(
                source_artifact_dir=str(root),
                provenance_json=str(provenance_path),
                pr_number=395,
                repository=REPOSITORY,
                expected_head_sha=HEAD_SHA,
                current_base_sha=BASE_SHA,
                source_helper_blob_sha=helper_sha,
                current_helper_blob_sha=helper_sha,
                control_sha=CONTROL_SHA,
                expected_macos_version="15.7.9",
                output=str(report_path),
            )
            compile_result = subprocess.CompletedProcess(
                ["/usr/bin/swiftc"], 0, stdout="", stderr="",
            )
            with (
                patch.object(
                    replay, "collect_runner",
                    return_value={
                        "os": "macOS", "os_version": "15.7.9", "architecture": "arm64",
                    },
                ),
                patch.object(
                    replay.subprocess, "run",
                    side_effect=[compile_result, OSError("permission denied")],
                ),
            ):
                result = replay.run_replay(args)

            report = json.loads(report_path.read_text(encoding="utf-8"))

        self.assertEqual(result, 1)
        self.assertEqual(report["product_acceptance"], "not_evaluated")
        self.assertEqual(report["helper"]["compile_exit_code"], 0)
        self.assertIsNotNone(report["ocr"]["completed_at"])
        self.assertIsNone(report["ocr"]["exit_code"])
        self.assertEqual(report["ocr"]["stderr"], "permission denied")


class WorkflowWiringTests(unittest.TestCase):
    def test_replay_is_read_only_and_does_not_checkout_or_launch_hane(self):
        workflow = (REPOSITORY_ROOT / ".github/workflows/aadw-gui-validation.yml").read_text(encoding="utf-8")
        self.assertIn("actions: read", workflow)
        self.assertIn("hosted-gui-ocr-replay/1", workflow)
        self.assertIn("actions/download-artifact@v5", workflow)
        self.assertIn("scripts/hosted_gui_ocr_replay.py replay", workflow)
        self.assertIn(
            "if: ${{ steps.preflight.outputs.validation_kind != 'ocr-replay' }}\n        uses: actions/checkout@v6",
            workflow,
        )
        self.assertIn(
            "- name: Validate Hane using the trusted procedure\n        if: ${{ steps.preflight.outputs.validation_kind != 'ocr-replay' }}",
            workflow,
        )
        self.assertIn("steps.preflight.outputs.validation_kind == 'ocr-replay'", workflow)
        self.assertIn("Upload OCR replay evidence", workflow)
        replay_source = MODULE_PATH.read_text(encoding="utf-8")
        self.assertEqual(
            replay.SOURCE_IMAGE.as_posix(),
            "scroll_inertia/direction-calibration-final-reset.png",
        )
        self.assertIn('"ocr"', replay_source)
        self.assertIn('"product_acceptance": "not_evaluated"', replay_source)


if __name__ == "__main__":
    unittest.main()
