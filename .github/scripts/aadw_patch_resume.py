#!/usr/bin/env python3
"""Validate a trusted request to continue from an unapproved worker patch.

The restored patch remains untrusted input. This module only checks provenance,
exact-head binding, and the narrow Issue #510 U2 file scope.
"""
from __future__ import annotations

import argparse
from datetime import datetime
import hashlib
import json
from pathlib import Path
from pathlib import PurePosixPath
import re
import sys

from aadw_action_contract import parse_request

SOURCE_FIELDS = {
    "run_id": "AADW_PATCH_SOURCE_RUN_ID",
    "run_attempt": "AADW_PATCH_SOURCE_RUN_ATTEMPT",
    "artifact_id": "AADW_PATCH_SOURCE_ARTIFACT_ID",
    "artifact_sha256": "AADW_PATCH_SOURCE_ARTIFACT_SHA256",
    "source_comment_id": "AADW_PATCH_SOURCE_COMMENT_ID",
    "attestation_comment_id": "AADW_PATCH_SOURCE_ATTESTATION_COMMENT_ID",
    "mode": "AADW_PATCH_SOURCE_MODE",
}
SHA256 = re.compile(r"[0-9a-f]{64}")
ATTESTATION = re.compile(
    r"^AADW_PATCH_SOURCE_ATTESTED: run_id=(\d+) attempt=(\d+) artifact_id=(\d+) "
    r"sha256=([0-9a-f]{64}) pr=(\d+) head=([0-9a-f]{40}) "
    r"source_comment=(\d+) status=unapplied$"
)

# Files that belong to the bounded Issue #510 U2 release-pipeline PR. The
# trusted workflow itself, its contract, and repository authority files are not
# included. A future U2 file addition requires a reviewed policy change.
U2_ALLOWED_PATHS = {
    ".github/RELEASE_AUTOMATION.md",
    ".github/workflows/ci.yml",
    ".github/workflows/release-request.yml",
    ".github/workflows/windows-release.yml",
    "README.md",
    "crates/update/src/trust.rs",
    "crates/updater/build.rs",
    "crates/updater/src/main.rs",
    "docs/implementation/issue-510-u2-release-pipeline-handoff.md",
    "docs/windows-release-signing.md",
    "scripts/bundle-macos.sh",
    "scripts/release-requirements.txt",
    "scripts/release_catalog.py",
    "scripts/tests/test_release_catalog.py",
    "scripts/tests/test_release_workflows.py",
    "scripts/tests/test_windows_zip_layout.py",
    "windows/shell-extension/Trust-HaneShellCertificate.ps1",
}

U2_CI_ADDITIONS = [
    "      - name: Install release tooling",
    "        uses: actions/setup-python@v6",
    "        with:",
    '          python-version: "3.12"',
    "",
    "      - name: Install release tooling requirements",
    "        run: python -m pip install -r scripts/release-requirements.txt",
    "",
    "      - name: Test Windows ZIP layout contract",
    "        run: python scripts/tests/test_windows_zip_layout.py",
    "",
    "      - name: Test update release pipeline fixtures",
    "        run: python scripts/tests/test_release_catalog.py",
    "",
    "      - name: Test update release workflow wiring",
    "        run: python scripts/tests/test_release_workflows.py",
    "",
]


def _one(body: str, name: str, *, required: bool = True) -> str | None:
    values = [
        line[len(name) + 1 :].strip()
        for line in body.splitlines()
        if line.startswith(name + ":")
    ]
    if not values and not required:
        return None
    if len(values) != 1 or not values[0]:
        raise ValueError(f"{name} must appear exactly once with a value")
    return values[0]


def parse_resume_request(body: str) -> dict | None:
    lines = body.splitlines()
    present = [field for field in SOURCE_FIELDS.values() if any(line.startswith(field + ":") for line in lines)]
    if not present:
        return None
    if len(present) != len(SOURCE_FIELDS):
        raise ValueError("successful-patch continuation requires every source field")
    if any(line.startswith(("AADW_CHECKPOINT_RUN_ID:", "AADW_CHECKPOINT_RUN_ATTEMPT:")) for line in lines):
        raise ValueError("failed-run checkpoints and successful-patch continuation cannot be combined")

    values = {key: _one(body, name) for key, name in SOURCE_FIELDS.items()}
    for key in ("run_id", "run_attempt", "artifact_id", "source_comment_id", "attestation_comment_id"):
        if not values[key].isdigit() or int(values[key]) <= 0:
            raise ValueError(f"{SOURCE_FIELDS[key]} must be a positive integer")
        values[key] = int(values[key])
    values["artifact_sha256"] = values["artifact_sha256"].lower()
    if not SHA256.fullmatch(values["artifact_sha256"]):
        raise ValueError("artifact SHA256 must be 64 hexadecimal characters")
    if values["mode"] not in {"manifest", "legacy_attested"}:
        raise ValueError("patch source mode must be manifest or legacy_attested")
    return values


def _time(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def _issue_api_url(repository: str, pr_number: int) -> str:
    return f"https://api.github.com/repos/{repository}/issues/{pr_number}"


def _parse_attestation(body: str) -> dict:
    lines = [line for line in body.splitlines() if line.startswith("AADW_PATCH_SOURCE_ATTESTED:")]
    if len(lines) != 1:
        raise ValueError("source attestation must contain exactly one machine-readable line")
    match = ATTESTATION.fullmatch(lines[0])
    if not match:
        raise ValueError("source attestation line is malformed")
    keys = ("run_id", "run_attempt", "artifact_id", "artifact_sha256", "pr_number", "target_sha", "source_comment_id")
    result = dict(zip(keys, match.groups(), strict=True))
    for key in ("run_id", "run_attempt", "artifact_id", "pr_number", "source_comment_id"):
        result[key] = int(result[key])
    return result


def _step_succeeded(job: dict, name: str) -> bool:
    return any(step.get("name") == name and step.get("conclusion") == "success" for step in job.get("steps", []))


def validate_source(
    request: dict,
    run: dict,
    jobs: dict,
    artifacts: dict,
    source_comment: dict,
    attestation_comment: dict,
    *,
    repository: str,
    pr_number: int,
    target_sha: str,
    actor: str,
) -> dict:
    if run.get("id") != request["run_id"] or run.get("run_attempt") != request["run_attempt"]:
        raise ValueError("source run ID or attempt does not match the request")
    if run.get("repository", {}).get("full_name") != repository:
        raise ValueError("source run belongs to a different repository")
    if run.get("path") != ".github/workflows/claude-fix.yml" or run.get("event") != "issue_comment":
        raise ValueError("source must be an AADW Claude Fix issue-comment run")
    workflow_sha = run.get("head_sha", "").lower()
    if run.get("head_branch") != "main" or not re.fullmatch(r"[0-9a-f]{40}", workflow_sha):
        raise ValueError("source run must identify the trusted default-branch workflow revision")
    if run.get("status") != "completed" or run.get("conclusion") != "cancelled":
        raise ValueError("source run must be cancelled after its worker completed")
    if run.get("actor", {}).get("login") != actor:
        raise ValueError("source run actor does not match the trusted requester")

    issue_url = _issue_api_url(repository, pr_number)
    if source_comment.get("id") != request["source_comment_id"]:
        raise ValueError("source comment ID does not match the request")
    if source_comment.get("issue_url") != issue_url or source_comment.get("user", {}).get("login") != actor:
        raise ValueError("source handoff must be authored by the trusted requester on this PR")
    source_handoff = parse_request(source_comment.get("body", ""))
    if source_handoff.get("action") != "implement" or source_handoff.get("target_sha") != target_sha:
        raise ValueError("source handoff does not target this exact PR head")
    if abs((_time(run["created_at"]) - _time(source_comment["created_at"])).total_seconds()) > 300:
        raise ValueError("source run is not temporally bound to the selected handoff comment")

    job_list = jobs.get("jobs", [])
    worker = next((job for job in job_list if job.get("name") == "worker"), None)
    finalizer = next((job for job in job_list if job.get("name") == "finalize"), None)
    if worker is None or worker.get("conclusion") != "success":
        raise ValueError("source worker did not complete successfully")
    if not _step_succeeded(worker, "Confirm Claude used Sonnet 5.5"):
        raise ValueError("source worker did not verify Claude Sonnet 5.5")
    if not _step_succeeded(worker, "Upload completed worker patch"):
        raise ValueError("source worker did not upload a completed patch")
    if finalizer is None or finalizer.get("conclusion") != "cancelled":
        raise ValueError("source patch must not have been finalized")

    expected_name = f"aadw-worker-patch-{request['run_id']}"
    matches = [
        item for item in artifacts.get("artifacts", [])
        if item.get("name") == expected_name
        and item.get("workflow_run", {}).get("id") == request["run_id"]
    ]
    if len(matches) != 1:
        raise ValueError("source run must have exactly one completed worker patch artifact")
    artifact = matches[0]
    if artifact.get("id") != request["artifact_id"]:
        raise ValueError("source artifact ID does not match the request")
    if artifact.get("expired") is not False:
        raise ValueError("source artifact has expired")
    if artifact.get("digest") != "sha256:" + request["artifact_sha256"]:
        raise ValueError("source artifact digest does not match GitHub metadata")

    if attestation_comment.get("id") != request["attestation_comment_id"]:
        raise ValueError("source attestation comment ID does not match the request")
    if attestation_comment.get("issue_url") != issue_url or attestation_comment.get("user", {}).get("login") != actor:
        raise ValueError("source attestation must be authored by the trusted requester on this PR")
    attestation = _parse_attestation(attestation_comment.get("body", ""))
    expected_attestation = {
        "run_id": request["run_id"],
        "run_attempt": request["run_attempt"],
        "artifact_id": request["artifact_id"],
        "artifact_sha256": request["artifact_sha256"],
        "pr_number": pr_number,
        "target_sha": target_sha,
        "source_comment_id": request["source_comment_id"],
    }
    if attestation != expected_attestation:
        raise ValueError("source attestation does not match the selected run, artifact, PR, and head")
    if "未適用" not in attestation_comment.get("body", "") or "承認ではありません" not in attestation_comment.get("body", ""):
        raise ValueError("source attestation must explicitly state that the patch is unapplied and not approved")

    return {
        "artifact_name": expected_name,
        "artifact_id": artifact["id"],
        "artifact_sha256": request["artifact_sha256"],
        "run_id": request["run_id"],
        "run_attempt": request["run_attempt"],
        "source_comment_id": request["source_comment_id"],
        "attestation_comment_id": request["attestation_comment_id"],
        "mode": request["mode"],
        "repository": repository,
        "pr_number": pr_number,
        "target_sha": target_sha,
        "root_cause": source_handoff["root_cause"],
        "actor": actor,
        "workflow_sha": workflow_sha,
    }


def validate_patch_bundle(patch_path: Path, manifest_path: Path, source: dict) -> None:
    if patch_path.is_symlink() or not patch_path.is_file() or not (0 < patch_path.stat().st_size <= 16 * 1024 * 1024):
        raise ValueError("source patch must be a bounded nonempty regular file")
    if manifest_path.is_symlink():
        raise ValueError("source patch manifest must not be a symlink")
    if manifest_path.exists():
        if manifest_path.is_symlink() or not manifest_path.is_file() or manifest_path.stat().st_size > 65536:
            raise ValueError("source patch manifest must be a bounded regular file")
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        expected = {
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
            "patch_sha256": hashlib.sha256(patch_path.read_bytes()).hexdigest(),
        }
        if manifest != expected:
            raise ValueError("source patch manifest does not match its run or contents")
    elif source["mode"] != "legacy_attested":
        raise ValueError("source patch manifest is required for new continuation artifacts")


def is_u2_release_path(path: str) -> bool:
    # Git uses "/" as its only directory separator. A literal backslash is a
    # different filename on the Linux finalizer and must not alias an allowed path.
    if "\\" in path:
        return False
    parsed = PurePosixPath(path)
    if parsed.is_absolute() or any(part in {"", ".", ".."} for part in parsed.parts):
        return False
    return path in U2_ALLOWED_PATHS


def validate_u2_paths(paths: list[str]) -> None:
    if not paths:
        raise ValueError("U2 patch changes no files")
    rejected = [path for path in paths if not is_u2_release_path(path)]
    if rejected:
        raise ValueError("U2 patch changed a file outside its allowlist: " + ", ".join(rejected))


def validate_u2_ci_diff(diff: str) -> None:
    added: list[str] = []
    removed: list[str] = []
    for line in diff.splitlines():
        if line.startswith(("+++ ", "--- ")) or line.startswith(("diff --git ", "index ", "@@")):
            continue
        if line.startswith("+"):
            added.append(line[1:])
        elif line.startswith("-"):
            removed.append(line[1:])
    if removed or added != U2_CI_ADDITIONS:
        raise ValueError("Issue #510 U2 CI changes must add only the approved release fixture steps")


def validate_u2_ci_workflow(workflow: str) -> None:
    top_level_permissions = re.search(
        r"(?m)^permissions:\n  contents: read\n  pull-requests: read\n\njobs:\n",
        workflow,
    )
    if top_level_permissions is None:
        raise ValueError("CI must retain the repository's read-only top-level permissions")

    job_start = re.search(r"(?m)^  rust_ci_changes:\s*$", workflow)
    if job_start is None:
        raise ValueError("CI must keep update fixture tests in rust_ci_changes")
    body_start = workflow.find("\n", job_start.start()) + 1
    next_job = re.search(r"(?m)^  [A-Za-z0-9_-]+:\s*$", workflow[body_start:])
    job_end = body_start + next_job.start() if next_job else len(workflow)
    job = workflow[body_start:job_end]
    if re.search(r"(?m)^    permissions:", job):
        raise ValueError("rust_ci_changes must inherit read-only top-level permissions")

    expected = (
        "      - name: Test release PR policy\n"
        "        run: python3 .github/tests/test_release_pr.py\n"
        "\n"
        + "\n".join(U2_CI_ADDITIONS).rstrip("\n")
        + "\n\n"
        + "      - name: Detect Rust CI inputs\n"
    )
    if job.count(expected) != 1:
        raise ValueError("Release fixture tests must stay in read-only rust_ci_changes")


def read_json(path: str) -> dict:
    file = Path(path)
    if file.is_symlink() or not file.is_file() or file.stat().st_size > 16 * 1024 * 1024:
        raise ValueError("input must be a bounded regular JSON file")
    value = json.loads(file.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError("input JSON must be an object")
    return value


def main() -> None:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    handoff = sub.add_parser("parse-handoff")
    handoff.add_argument("body_file")
    verify = sub.add_parser("verify-source")
    for name in ("request", "run", "jobs", "artifacts", "source_comment", "attestation_comment"):
        verify.add_argument("--" + name.replace("_", "-"), required=True)
    verify.add_argument("--repository", required=True)
    verify.add_argument("--pr-number", type=int, required=True)
    verify.add_argument("--target-sha", required=True)
    verify.add_argument("--actor", required=True)
    bundle = sub.add_parser("verify-bundle")
    bundle.add_argument("--patch", required=True)
    bundle.add_argument("--manifest", required=True)
    bundle.add_argument("--source", required=True)
    manifest = sub.add_parser("create-manifest")
    manifest.add_argument("--patch", required=True)
    manifest.add_argument("--output", required=True)
    for name in ("repository", "pr-number", "target-sha", "run-id", "run-attempt", "comment-id", "root-cause", "workflow-sha", "actor"):
        manifest.add_argument("--" + name, required=True)
    sub.add_parser("u2-paths")
    sub.add_parser("u2-ci-diff")
    sub.add_parser("u2-ci-workflow")
    args = parser.parse_args()

    if args.command == "parse-handoff":
        body_path = Path(args.body_file)
        if body_path.is_symlink() or not body_path.is_file() or body_path.stat().st_size > 65536:
            raise ValueError("Commander handoff must be a bounded regular file")
        result = parse_resume_request(body_path.read_text(encoding="utf-8"))
        print(json.dumps(result or {"enabled": False}, ensure_ascii=False))
    elif args.command == "verify-source":
        request = read_json(args.request)
        result = validate_source(
            request, read_json(args.run), read_json(args.jobs), read_json(args.artifacts),
            read_json(args.source_comment), read_json(args.attestation_comment),
            repository=args.repository, pr_number=args.pr_number,
            target_sha=args.target_sha.lower(), actor=args.actor,
        )
        print(json.dumps(result, ensure_ascii=False))
    elif args.command == "verify-bundle":
        validate_patch_bundle(Path(args.patch), Path(args.manifest), read_json(args.source))
    elif args.command == "create-manifest":
        patch = Path(args.patch)
        if patch.is_symlink() or not patch.is_file() or not (0 < patch.stat().st_size <= 16 * 1024 * 1024):
            raise ValueError("worker patch must be a bounded nonempty regular file")
        value = {
            "schema": 1,
            "repository": args.repository,
            "pr_number": int(args.pr_number),
            "target_sha": args.target_sha.lower(),
            "run_id": int(args.run_id),
            "run_attempt": int(args.run_attempt),
            "trigger_comment_id": int(args.comment_id),
            "root_cause": args.root_cause,
            "workflow_sha": args.workflow_sha,
            "actor": args.actor,
            "model": "claude-sonnet-5-5",
            "model_verified": True,
            "patch_sha256": hashlib.sha256(patch.read_bytes()).hexdigest(),
        }
        Path(args.output).write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    elif args.command == "u2-paths":
        raw = sys.stdin.buffer.read()
        paths = [item.decode("utf-8") for item in raw.split(b"\0") if item]
        validate_u2_paths(paths)
    elif args.command == "u2-ci-diff":
        validate_u2_ci_diff(sys.stdin.read())
    else:
        validate_u2_ci_workflow(sys.stdin.read())


if __name__ == "__main__":
    main()
