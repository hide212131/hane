import importlib.util
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
spec = importlib.util.spec_from_file_location(
    "aadw_patch_resume", ROOT / "scripts/aadw_patch_resume.py"
)
resume = importlib.util.module_from_spec(spec)
spec.loader.exec_module(resume)

WORKFLOW = (ROOT / "workflows/claude-fix.yml").read_text()
CI_WORKFLOW = (ROOT / "workflows/ci.yml").read_text()
HEAD = "2a271832592c1668adc6c4c1ede15f8b09622176"
ARTIFACT_SHA = "03fd9cc2ecf718ba3ae3eb47a2bc39f251c536a87bf2219f4456ad67c4dd6936"
ISSUE_URL = "https://api.github.com/repos/hide212131/hane/issues/526"
WORKFLOW_SHA = "b835a6e090a2241e71a89ef8b5222e934d9ed0d1"


def handoff(head=HEAD):
    return "\n".join(
        [
            "@claude",
            "AADW_COMMANDER_HANDOFF_V3",
            "AADW_ACTION: implement",
            f"AADW_TARGET_HEAD: {head}",
            "AADW_FAILURE_CLASS: product",
            "AADW_ROOT_CAUSE: issue-510-u2-catalog-sequence-monotonicity",
            "AADW_EVIDENCE: https://github.com/hide212131/hane/pull/526#issuecomment-6266583410",
        ]
    )


def source_fixture():
    request = {
        "run_id": 39248051447,
        "run_attempt": 1,
        "artifact_id": 12185386128,
        "artifact_sha256": ARTIFACT_SHA,
        "source_comment_id": 6266597849,
        "attestation_comment_id": 6268082334,
        "mode": "legacy_attested",
    }
    run = {
        "id": request["run_id"],
        "run_attempt": request["run_attempt"],
        "repository": {"full_name": "hide212131/hane"},
        "path": ".github/workflows/claude-fix.yml",
        "event": "issue_comment",
        "head_branch": "main",
        "head_sha": WORKFLOW_SHA,
        "status": "completed",
        "conclusion": "cancelled",
        "actor": {"login": "hide212131"},
        "created_at": "2026-10-19T11:54:36Z",
    }
    jobs = {
        "jobs": [
            {
                "name": "worker",
                "conclusion": "success",
                "steps": [
                    {"name": "Confirm Claude used Sonnet 5.5", "conclusion": "success"},
                    {"name": "Upload completed worker patch", "conclusion": "success"},
                ],
            },
            {"name": "finalize", "conclusion": "cancelled", "steps": []},
        ]
    }
    artifacts = {
        "artifacts": [
            {
                "id": request["artifact_id"],
                "name": f"aadw-worker-patch-{request['run_id']}",
                "digest": "sha256:" + ARTIFACT_SHA,
                "expired": False,
                "workflow_run": {"id": request["run_id"]},
            }
        ]
    }
    source_comment = {
        "id": request["source_comment_id"],
        "issue_url": ISSUE_URL,
        "user": {"login": "hide212131"},
        "created_at": "2026-10-19T11:54:34Z",
        "body": handoff(),
    }
    attestation_comment = {
        "id": request["attestation_comment_id"],
        "issue_url": ISSUE_URL,
        "user": {"login": "hide212131"},
        "body": (
            "## 保存済み案の出典（未適用）\n\n"
            "PRへの適用や公開の承認ではありません。\n\n"
            "AADW_PATCH_SOURCE_ATTESTED: run_id=39248051447 attempt=1 "
            "artifact_id=12185386128 sha256="
            f"{ARTIFACT_SHA} pr=526 head={HEAD} "
            "source_comment=6266597849 status=unapplied"
        ),
    }
    return request, run, jobs, artifacts, source_comment, attestation_comment


class HandoffParsingTests(unittest.TestCase):
    def test_no_resume_fields_leaves_normal_request_unchanged(self):
        self.assertIsNone(resume.parse_resume_request(handoff()))

    def test_all_source_fields_parse_once(self):
        body = handoff() + "\n" + "\n".join(
            [
                "AADW_PATCH_SOURCE_RUN_ID: 39248051447",
                "AADW_PATCH_SOURCE_RUN_ATTEMPT: 1",
                "AADW_PATCH_SOURCE_ARTIFACT_ID: 12185386128",
                f"AADW_PATCH_SOURCE_ARTIFACT_SHA256: {ARTIFACT_SHA}",
                "AADW_PATCH_SOURCE_COMMENT_ID: 6266597849",
                "AADW_PATCH_SOURCE_ATTESTATION_COMMENT_ID: 6268082334",
                "AADW_PATCH_SOURCE_MODE: legacy_attested",
            ]
        )
        parsed = resume.parse_resume_request(body)
        self.assertEqual(parsed["run_id"], 39248051447)
        self.assertEqual(parsed["mode"], "legacy_attested")

    def test_partial_duplicate_and_mixed_checkpoint_requests_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "every source field"):
            resume.parse_resume_request(handoff() + "\nAADW_PATCH_SOURCE_RUN_ID: 42")
        complete = handoff() + "\n" + "\n".join(
            [
                "AADW_PATCH_SOURCE_RUN_ID: 1",
                "AADW_PATCH_SOURCE_RUN_ATTEMPT: 1",
                "AADW_PATCH_SOURCE_ARTIFACT_ID: 1",
                f"AADW_PATCH_SOURCE_ARTIFACT_SHA256: {ARTIFACT_SHA}",
                "AADW_PATCH_SOURCE_COMMENT_ID: 1",
                "AADW_PATCH_SOURCE_ATTESTATION_COMMENT_ID: 2",
                "AADW_PATCH_SOURCE_MODE: legacy_attested",
            ]
        )
        with self.assertRaisesRegex(ValueError, "exactly once"):
            resume.parse_resume_request(complete + "\nAADW_PATCH_SOURCE_RUN_ID: 2")
        with self.assertRaisesRegex(ValueError, "cannot be combined"):
            resume.parse_resume_request(complete + "\nAADW_CHECKPOINT_RUN_ID: 5")


class SourceProvenanceTests(unittest.TestCase):
    def _validate(self, values=None):
        request, run, jobs, artifacts, source_comment, attestation = source_fixture()
        if values:
            request, run, jobs, artifacts, source_comment, attestation = values(
                request, run, jobs, artifacts, source_comment, attestation
            )
        return resume.validate_source(
            request,
            run,
            jobs,
            artifacts,
            source_comment,
            attestation,
            repository="hide212131/hane",
            pr_number=526,
            target_sha=HEAD,
            actor="hide212131",
        )

    def test_valid_run_artifact_and_unapplied_attestation_are_bound(self):
        source = self._validate()
        self.assertEqual(source["artifact_id"], 12185386128)
        self.assertEqual(source["target_sha"], HEAD)
        self.assertEqual(source["mode"], "legacy_attested")

    def test_wrong_head_actor_run_result_digest_or_expired_artifact_is_rejected(self):
        mutations = [
            lambda req, run, jobs, artifacts, comment, att: (req, run, jobs, artifacts, {**comment, "body": handoff("0" * 40)}, att),
            lambda req, run, jobs, artifacts, comment, att: (req, run, jobs, artifacts, {**comment, "user": {"login": "other"}}, att),
            lambda req, run, jobs, artifacts, comment, att: (req, {**run, "conclusion": "success"}, jobs, artifacts, comment, att),
            lambda req, run, jobs, artifacts, comment, att: (req, run, {"jobs": [jobs["jobs"][0], {"name": "finalize", "conclusion": "success"}]}, artifacts, comment, att),
            lambda req, run, jobs, artifacts, comment, att: (req, run, jobs, {"artifacts": [{**artifacts["artifacts"][0], "expired": True}]}, comment, att),
            lambda req, run, jobs, artifacts, comment, att: (req, run, jobs, {"artifacts": [{**artifacts["artifacts"][0], "digest": "sha256:" + "0" * 64}]}, comment, att),
            lambda req, run, jobs, artifacts, comment, att: (req, run, jobs, artifacts, comment, {**att, "id": 99}),
        ]
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                with self.assertRaises(ValueError):
                    self._validate(mutate)


class PatchAndScopeTests(unittest.TestCase):
    def test_legacy_artifact_requires_attestation_but_can_lack_manifest(self):
        source = self._validate_source()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            patch = root / "aadw-worker.patch"
            patch.write_bytes(b"trusted provenance does not make patch trusted\n")
            resume.validate_patch_bundle(patch, root / "missing.json", source)
            source["mode"] = "manifest"
            with self.assertRaisesRegex(ValueError, "manifest is required"):
                resume.validate_patch_bundle(patch, root / "missing.json", source)

    def test_new_manifest_binds_patch_hash_and_request(self):
        source = self._validate_source()
        source["mode"] = "manifest"
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            patch = root / "aadw-worker.patch"
            patch.write_bytes(b"patch bytes\n")
            manifest = root / "aadw-worker-manifest.json"
            manifest.write_text(
                json.dumps(
                    {
                        "schema": 1,
                        "repository": source["repository"],
                        "pr_number": source["pr_number"],
                        "target_sha": source["target_sha"],
                        "run_id": source["run_id"],
                        "run_attempt": source["run_attempt"],
                        "trigger_comment_id": source["source_comment_id"],
                        "root_cause": source["root_cause"],
                        "workflow_sha": source["workflow_sha"],
                        "actor": source["actor"],
                        "model": "claude-sonnet-5-5",
                        "model_verified": True,
                        "patch_sha256": hashlib.sha256(patch.read_bytes()).hexdigest(),
                    }
                ),
                encoding="utf-8",
            )
            resume.validate_patch_bundle(patch, manifest, source)
            patch.write_bytes(b"changed after manifest\n")
            with self.assertRaisesRegex(ValueError, "does not match"):
                resume.validate_patch_bundle(patch, manifest, source)

    def test_u2_path_allowlist_is_exact_and_blocks_trusted_controls(self):
        resume.validate_u2_paths(
            [
                ".github/workflows/windows-release.yml",
                "crates/updater/src/main.rs",
                "scripts/tests/test_release_catalog.py",
            ]
        )
        for path in [
            ".github/workflows/claude-fix.yml",
            ".github/scripts/aadw_patch_resume.py",
            ".github/tests/test_aadw_patch_resume.py",
            "docs/aadw-command-policy.md",
            "crates/updater/src/../.github/workflows/evil.yml",
            "src/unrelated.rs",
            r".github\workflows\windows-release.yml",
            r"crates\updater\src\main.rs",
        ]:
            with self.subTest(path=path):
                with self.assertRaisesRegex(ValueError, "outside its allowlist"):
                    resume.validate_u2_paths([path])
        with self.assertRaisesRegex(ValueError, "outside its allowlist"):
            resume.validate_u2_paths(["src/unrelated.rs", "README.md"])

    def test_ci_changes_are_limited_to_release_fixture_steps(self):
        diff = "diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml\n"
        diff += "@@ -108,0 +109,17 @@\n"
        diff += "\n".join("+" + line for line in resume.U2_CI_ADDITIONS)
        resume.validate_u2_ci_diff(diff)
        with self.assertRaisesRegex(ValueError, "only the approved"):
            resume.validate_u2_ci_diff(diff + "\n+      - run: echo unsafe")
        with self.assertRaisesRegex(ValueError, "only the approved"):
            resume.validate_u2_ci_diff(diff + "\n-      - run: remove-check")

    def test_ci_fixture_steps_stay_in_read_only_rust_job(self):
        inserted_steps = "\n".join(resume.U2_CI_ADDITIONS).rstrip("\n") + "\n\n"
        test_before = (
            "      - name: Test release PR policy\n"
            "        run: python3 .github/tests/test_release_pr.py\n"
            "\n"
        )
        detect_after = "      - name: Detect Rust CI inputs\n        id: filter\n"
        prefix = "permissions:\n  contents: read\n  pull-requests: read\n\njobs:\n"
        valid = (
            prefix
            + "  rust_ci_changes:\n    runs-on: ubuntu-latest\n    steps:\n"
            + test_before
            + inserted_steps
            + detect_after
            + "  release_checks:\n    runs-on: ubuntu-latest\n    steps:\n"
        )
        resume.validate_u2_ci_workflow(valid)

        moved = (
            prefix
            + "  rust_ci_changes:\n    runs-on: ubuntu-latest\n    steps:\n"
            + test_before
            + detect_after
            + "  release_checks:\n    runs-on: ubuntu-latest\n    steps:\n"
            + inserted_steps
        )
        with self.assertRaisesRegex(ValueError, "read-only rust_ci_changes"):
            resume.validate_u2_ci_workflow(moved)

        writable = valid.replace(
            "  rust_ci_changes:\n    runs-on: ubuntu-latest\n",
            "  rust_ci_changes:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: write\n",
        )
        with self.assertRaisesRegex(ValueError, "inherit read-only top-level permissions"):
            resume.validate_u2_ci_workflow(writable)

    def _validate_source(self):
        request, run, jobs, artifacts, source_comment, attestation = source_fixture()
        return resume.validate_source(
            request,
            run,
            jobs,
            artifacts,
            source_comment,
            attestation,
            repository="hide212131/hane",
            pr_number=526,
            target_sha=HEAD,
            actor="hide212131",
        )


class WorkflowWiringTests(unittest.TestCase):
    def test_continuation_is_exact_artifact_id_and_head_bound(self):
        self.assertIn("actions: read", WORKFLOW)
        self.assertIn("attempts/${source_run_attempt}/jobs", WORKFLOW)
        self.assertIn("artifact-ids: ${{ needs.prepare.outputs.patch_source_artifact_id }}", WORKFLOW)
        self.assertIn("python3 trusted-model-verifier/.github/scripts/aadw_patch_resume.py create-manifest", WORKFLOW)
        self.assertIn("git -C pr-head diff --cached --no-renames --name-only -z", WORKFLOW)
        self.assertIn("git -C final-head diff --cached --no-renames --name-only -z", WORKFLOW)
        self.assertIn("--target-sha \"$expected_sha\"", WORKFLOW)
        self.assertIn("verify-bundle", WORKFLOW)
        self.assertIn("git -C pr-head apply --check --whitespace=nowarn", WORKFLOW)
        self.assertIn("Successful-patch continuation status:", WORKFLOW)

    def test_u2_write_scope_keeps_separate_owner_and_exact_patch_approval(self):
        self.assertIn("hane-aadw-trusted-workflow-write", WORKFLOW)
        self.assertIn("git -C final-head diff --cached --no-renames --name-only -z", WORKFLOW)
        self.assertIn("u2-ci-diff", WORKFLOW)
        self.assertIn("u2-ci-workflow", WORKFLOW)
        self.assertIn("AADW_WORKFLOW_PATCH_APPROVED: run_id=${RUN_ID} sha256=${patch_sha256}", WORKFLOW)
        self.assertNotIn("EXPECTED_ROOT_CAUSE", WORKFLOW)
        self.assertNotIn("issue-510-u2-release-pipeline-custom-crypto", WORKFLOW)
        self.assertIn("Test AADW successful patch continuation provenance", CI_WORKFLOW)


if __name__ == "__main__":
    unittest.main()
