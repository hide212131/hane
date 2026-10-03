#!/usr/bin/env python3
"""Read-only OCR replay for one screenshot saved by the trusted GUI workflow.

This procedure reports what Vision recognized in an existing image. It never
launches Hane and never judges product acceptance.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import platform
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from typing import Any


PROCEDURE_VERSION = "hosted-gui-ocr-replay/1"
SOURCE_PROCEDURE = "hosted-scroll-inertia/12"
SOURCE_WORKFLOW = ".github/workflows/aadw-gui-validation.yml"
SOURCE_IMAGE = Path("scroll_inertia/direction-calibration-final-reset.png")
SHA_RE = re.compile(r"[0-9a-f]{40}")
VISIBLE_LINE_RE = re.compile(r"\bLINE\s+(\d+)\b", re.IGNORECASE)


class ReplayError(ValueError):
    """The requested image is not the exact trusted source artifact."""


def visible_lines(text: str) -> list[int]:
    """Return fixture line numbers recognized by the existing procedure."""
    return sorted({int(value) for value in VISIBLE_LINE_RE.findall(text)})


def _read_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ReplayError(f"expected a JSON object in {path.name}")
    return value


def _require_sha(value: Any, field: str) -> str:
    if not isinstance(value, str) or SHA_RE.fullmatch(value) is None:
        raise ReplayError(f"{field} is not a lowercase full 40-hex SHA")
    return value


def validate_source_metadata(
    run: dict[str, Any],
    artifact: dict[str, Any],
    *,
    source_run_id: int,
    artifact_id: int,
    pr_number: int,
    repository: str,
    default_branch: str,
) -> dict[str, Any]:
    """Validate GitHub's run and artifact records before downloading evidence."""
    if run.get("id") != source_run_id:
        raise ReplayError("source run id does not match the requested run")
    if run.get("status") != "completed":
        raise ReplayError("source workflow run is not completed")
    if run.get("event") != "workflow_dispatch":
        raise ReplayError("source run was not dispatched through the trusted workflow")
    if str(run.get("path", "")).split("@", 1)[0] != SOURCE_WORKFLOW:
        raise ReplayError("source run used a different workflow")
    if run.get("head_branch") != default_branch:
        raise ReplayError("source workflow did not run from the default branch")
    head_repository = run.get("head_repository")
    if not isinstance(head_repository, dict) or head_repository.get("full_name") != repository:
        raise ReplayError("source workflow belongs to a different repository")

    source_control_sha = _require_sha(run.get("head_sha"), "source workflow SHA")
    run_attempt = run.get("run_attempt")
    if not isinstance(run_attempt, int) or run_attempt < 1:
        raise ReplayError("source workflow attempt is invalid")

    expected_name = f"aadw-gui-evidence-pr-{pr_number}-{source_run_id}-{run_attempt}"
    if artifact.get("id") != artifact_id:
        raise ReplayError("source artifact id does not match the requested artifact")
    if artifact.get("name") != expected_name:
        raise ReplayError("source artifact name does not match the exact PR run")
    if artifact.get("expired") is not False:
        raise ReplayError("source artifact is expired or its expiry state is unknown")
    workflow_run = artifact.get("workflow_run")
    if not isinstance(workflow_run, dict) or workflow_run.get("id") != source_run_id:
        raise ReplayError("source artifact is not attached to the requested workflow run")

    return {
        "source_run_id": source_run_id,
        "source_run_attempt": run_attempt,
        "source_artifact_id": artifact_id,
        "source_artifact_name": expected_name,
        "source_workflow": SOURCE_WORKFLOW,
        "source_workflow_sha": source_control_sha,
        "source_event": "workflow_dispatch",
        "source_default_branch": default_branch,
    }


def validate_source_context(
    context: dict[str, Any],
    result: dict[str, Any],
    provenance: dict[str, Any],
    *,
    expected_pr_number: int,
    expected_repository: str,
    expected_head_sha: str,
) -> None:
    """Bind the PNG to the exact blocked scroll-inertia run and PR candidate."""
    if context.get("schema_version") != 1:
        raise ReplayError("source context schema is unsupported")
    if context.get("repository") != expected_repository:
        raise ReplayError("source context belongs to a different repository")
    if context.get("pr_number") != expected_pr_number:
        raise ReplayError("source context belongs to a different PR")
    if context.get("requested_head_sha") != expected_head_sha:
        raise ReplayError("source screenshot is not for the current PR head")
    _require_sha(context.get("requested_head_sha"), "source PR head")
    requested_base_sha = _require_sha(context.get("requested_base_sha"), "source PR base")
    if context.get("observed_base_sha") != requested_base_sha:
        raise ReplayError("source context did not verify its requested base")
    if context.get("execution_context") != "merge":
        raise ReplayError("source GUI run did not use the merge context")
    if context.get("procedure") != SOURCE_PROCEDURE:
        raise ReplayError("source artifact used a different GUI procedure")
    if str(context.get("workflow_run_id")) != str(provenance.get("source_run_id")):
        raise ReplayError("source context run id does not match GitHub metadata")
    if str(context.get("workflow_run_attempt")) != str(provenance.get("source_run_attempt")):
        raise ReplayError("source context attempt does not match GitHub metadata")
    if context.get("control_sha") != provenance.get("source_workflow_sha"):
        raise ReplayError("source context control SHA does not match the workflow run")
    _require_sha(context.get("execution_sha"), "source merge execution SHA")

    if result.get("overall_result") != "blocked":
        raise ReplayError("source GUI result was not blocked")
    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list):
        raise ReplayError("source GUI result has no scenario list")
    scroll_scenario = next(
        (item for item in scenarios if isinstance(item, dict) and item.get("name") == "scroll_inertia"),
        None,
    )
    if not isinstance(scroll_scenario, dict) or scroll_scenario.get("result") != "blocked":
        raise ReplayError("source scroll-inertia scenario is not the expected blocked run")
    steps = scroll_scenario.get("steps")
    if not isinstance(steps, list):
        raise ReplayError("source scroll-inertia result has no step list")
    calibration = next(
        (item for item in steps if isinstance(item, dict) and item.get("name") == "scroll_direction_calibration"),
        None,
    )
    if not isinstance(calibration, dict) or calibration.get("result") != "blocked":
        raise ReplayError("source run did not stop at direction calibration")
    observation_quality = result.get("observation_quality")
    if not isinstance(observation_quality, dict) or observation_quality.get("diagnosis_required") is not True:
        raise ReplayError("source result does not require measurement diagnosis")
    quality_steps = observation_quality.get("steps")
    required_measurement_steps = {"lines_coast", "direction_reversal", "pixels_direct_follow"}
    if not isinstance(quality_steps, list):
        raise ReplayError("source result has no observation-quality step classifications")
    classified_steps = {
        item.get("name"): item
        for item in quality_steps
        if isinstance(item, dict) and isinstance(item.get("name"), str)
    }
    for name in required_measurement_steps:
        item = classified_steps.get(name)
        if not isinstance(item, dict) or item.get("failure_class") != "measurement" or item.get("product_cause_proven") is not False:
            raise ReplayError(f"source step {name} is not classified as measurement-only")


def resolve_source_image(artifact_dir: Path) -> Path:
    root = artifact_dir.resolve(strict=True)
    image = (root / SOURCE_IMAGE).resolve(strict=True)
    try:
        image.relative_to(root)
    except ValueError as exc:
        raise ReplayError("source image resolves outside the downloaded artifact") from exc
    if not image.is_file() or image.suffix.lower() != ".png":
        raise ReplayError("source image is not a PNG file")
    return image


def _utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _command_output(command: list[str]) -> tuple[int, str]:
    try:
        completed = subprocess.run(command, capture_output=True, text=True, check=False, timeout=10)
    except (OSError, subprocess.TimeoutExpired) as exc:
        return 1, str(exc)
    return completed.returncode, completed.stdout.strip() if completed.returncode == 0 else completed.stderr.strip()


def collect_runner() -> dict[str, str]:
    os_status, os_version = _command_output(["/usr/bin/sw_vers", "-productVersion"])
    arch_status, architecture = _command_output(["/usr/bin/uname", "-m"])
    return {
        "os": "macOS" if os_status == 0 else platform.system(),
        "os_version": os_version if os_status == 0 else "unknown",
        "architecture": architecture if arch_status == 0 else platform.machine() or "unknown",
    }


def _write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def run_replay(args: argparse.Namespace) -> int:
    source_dir = Path(args.source_artifact_dir)
    provenance = _read_json(Path(args.provenance_json))
    context = _read_json(source_dir / "aadw-context.json")
    source_result = _read_json(source_dir / "result.json")
    validate_source_context(
        context,
        source_result,
        provenance,
        expected_pr_number=args.pr_number,
        expected_repository=args.repository,
        expected_head_sha=args.expected_head_sha,
    )
    image = resolve_source_image(source_dir)
    current_base_sha = _require_sha(args.current_base_sha, "current PR base")
    source_helper_blob_sha = _require_sha(args.source_helper_blob_sha, "source OCR helper blob")
    current_helper_blob_sha = _require_sha(args.current_helper_blob_sha, "current OCR helper blob")
    control_sha = _require_sha(args.control_sha, "replay control SHA")
    runner = collect_runner()

    report: dict[str, Any] = {
        "schema_version": 1,
        "procedure": PROCEDURE_VERSION,
        "purpose": "read_only_saved_image_ocr_replay",
        "product_acceptance": "not_evaluated",
        "source": {
            **provenance,
            "repository": args.repository,
            "pr_number": args.pr_number,
            "pr_head_sha": context["requested_head_sha"],
            "pr_base_sha": context["requested_base_sha"],
            "execution_sha": context["execution_sha"],
            "execution_context": context["execution_context"],
            "procedure": context["procedure"],
            "result": source_result["overall_result"],
            "reason": source_result.get("overall_reason"),
            "calibration_result": "blocked",
            "observation_quality": source_result["observation_quality"],
            "image_path": SOURCE_IMAGE.as_posix(),
            "image_sha256": _sha256(image),
        },
        "replay_context": {
            "current_pr_head_sha": args.expected_head_sha,
            "current_pr_base_sha": current_base_sha,
            "control_sha": control_sha,
            "same_pr_head_as_source": True,
            "same_base_as_source": current_base_sha == context["requested_base_sha"],
            "scope": "replays only the saved PNG; does not launch Hane or re-run the GUI scenario",
        },
        "runner": runner,
        "helper": {
            "source": "scripts/hosted_gui_interaction.swift",
            "source_blob_sha": source_helper_blob_sha,
            "replay_blob_sha": current_helper_blob_sha,
            "same_helper_source": source_helper_blob_sha == current_helper_blob_sha,
            "command": "ocr",
            "compile_exit_code": None,
            "compile_stderr": "",
        },
        "ocr": {
            "started_at": None,
            "completed_at": None,
            "exit_code": None,
            "recognized_text": "",
            "visible_lines": [],
            "line_001_detected": False,
            "stderr": "",
        },
    }

    reason = None
    if runner["os"] != "macOS" or runner["os_version"] != args.expected_macos_version:
        reason = f"runner OS mismatch: expected macOS {args.expected_macos_version}, observed {runner['os']} {runner['os_version']}"
    elif runner["architecture"] != "arm64":
        reason = f"runner architecture mismatch: expected arm64, observed {runner['architecture']}"
    elif source_helper_blob_sha != current_helper_blob_sha:
        reason = "OCR helper source differs from the source GUI run"

    if reason is not None:
        report["ocr"]["skip_reason"] = reason
        _write_json(Path(args.output), report)
        print(reason, file=sys.stderr)
        return 1

    helper_source = Path(__file__).resolve().with_name("hosted_gui_interaction.swift")
    if not helper_source.is_file():
        report["helper"]["compile_stderr"] = f"missing helper source: {helper_source.name}"
        _write_json(Path(args.output), report)
        return 1

    with tempfile.TemporaryDirectory(prefix="hane-gui-ocr-replay-") as temp_dir:
        helper_binary = Path(temp_dir) / "interaction-helper"
        try:
            compile_result = subprocess.run(
                ["/usr/bin/swiftc", str(helper_source), "-o", str(helper_binary)],
                capture_output=True,
                text=True,
                check=False,
                timeout=120,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            report["helper"]["compile_exit_code"] = 1
            report["helper"]["compile_stderr"] = str(exc)
            _write_json(Path(args.output), report)
            return 1
        report["helper"]["compile_exit_code"] = compile_result.returncode
        report["helper"]["compile_stderr"] = compile_result.stderr
        if compile_result.returncode == 0:
            report["ocr"]["started_at"] = _utc_now()
            try:
                ocr_result = subprocess.run(
                    [str(helper_binary), "ocr", str(image)],
                    capture_output=True,
                    text=True,
                    check=False,
                    timeout=30,
                )
                report["ocr"].update({
                    "completed_at": _utc_now(),
                    "exit_code": ocr_result.returncode,
                    "recognized_text": ocr_result.stdout,
                    "visible_lines": visible_lines(ocr_result.stdout),
                    "line_001_detected": 1 in visible_lines(ocr_result.stdout),
                    "stderr": ocr_result.stderr,
                })
            except subprocess.TimeoutExpired as exc:
                report["ocr"].update({
                    "completed_at": _utc_now(),
                    "exit_code": None,
                    "recognized_text": exc.stdout.decode(errors="replace") if isinstance(exc.stdout, bytes) else exc.stdout or "",
                    "stderr": exc.stderr.decode(errors="replace") if isinstance(exc.stderr, bytes) else exc.stderr or "",
                    "timed_out": True,
                })

    _write_json(Path(args.output), report)
    return 0 if report["helper"]["compile_exit_code"] == 0 and report["ocr"]["exit_code"] == 0 else 1


def validate_source_command(args: argparse.Namespace) -> int:
    provenance = validate_source_metadata(
        _read_json(Path(args.source_run_json)),
        _read_json(Path(args.source_artifact_json)),
        source_run_id=args.source_run_id,
        artifact_id=args.artifact_id,
        pr_number=args.pr_number,
        repository=args.repository,
        default_branch=args.default_branch,
    )
    _write_json(Path(args.output), provenance)
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    source = subparsers.add_parser("validate-source")
    source.add_argument("--source-run-json", required=True)
    source.add_argument("--source-artifact-json", required=True)
    source.add_argument("--source-run-id", required=True, type=int)
    source.add_argument("--artifact-id", required=True, type=int)
    source.add_argument("--pr-number", required=True, type=int)
    source.add_argument("--repository", required=True)
    source.add_argument("--default-branch", required=True)
    source.add_argument("--output", required=True)
    source.set_defaults(func=validate_source_command)

    replay = subparsers.add_parser("replay")
    replay.add_argument("--source-artifact-dir", required=True)
    replay.add_argument("--provenance-json", required=True)
    replay.add_argument("--source-helper-blob-sha", required=True)
    replay.add_argument("--current-helper-blob-sha", required=True)
    replay.add_argument("--control-sha", required=True)
    replay.add_argument("--repository", required=True)
    replay.add_argument("--pr-number", required=True, type=int)
    replay.add_argument("--expected-head-sha", required=True)
    replay.add_argument("--current-base-sha", required=True)
    replay.add_argument("--expected-macos-version", default="15.7.9")
    replay.add_argument("--output", required=True)
    replay.set_defaults(func=run_replay)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return args.func(args)
    except (OSError, json.JSONDecodeError, ReplayError, subprocess.TimeoutExpired) as exc:
        print(f"OCR replay failed closed: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
