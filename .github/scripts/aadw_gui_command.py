#!/usr/bin/env python3
"""Pure policy for the AADW hosted GUI comment router.

This module performs no network access. GitHub Actions fetches current facts and
passes them in; these helpers only parse and validate them deterministically.
"""

from __future__ import annotations

import argparse
import ast
import json
from pathlib import Path
import re
import sys
from typing import Any

# Keep the original comprehensive command grammar unchanged. The legacy
# `parse` CLI intentionally recognizes only this form so the comprehensive
# workflow cannot accept a focused command if it is ever mis-dispatched.
COMMAND_RE = re.compile(r"/gui-validate[ \t]+(head|merge)")
DATE_BADGE_COMMAND_RE = re.compile(r"/gui-validate[ \t]+date-badge[ \t]+(head|merge)")
NORMAL_LIST_COMMAND_RE = re.compile(r"/gui-validate[ \t]+normal-list[ \t]+(head|merge)")
SHA_RE = re.compile(r"[0-9a-f]{40}")
TRUSTED_PERMISSIONS = {"write", "maintain", "admin"}
RUN_NAME_PREFIX = "AADW GUI"

# A comment chooses only a small named validation kind and execution context.
# Workflow/script paths remain trusted default-branch data and are never read
# from comment arguments.
TRUSTED_ROUTES = {
    "comprehensive": {
        "workflow_file": "aadw-gui-validation.yml",
        "procedure_path": "scripts/hosted_gui_interaction.py",
    },
    "date-badge": {
        "workflow_file": "aadw-date-badge-gui-validation.yml",
        "procedure_path": "scripts/hosted_date_badge_gui.py",
    },
    "normal-list": {
        "workflow_file": "aadw-gui-validation.yml",
        "procedure_path": "scripts/hosted_normal_list_gui.py",
    },
    "sidebar-chrome": {
        "workflow_file": "aadw-sidebar-chrome-gui-validation.yml",
        "procedure_path": "scripts/hosted_sidebar_chrome_gui.py",
    },
}


class RequestError(ValueError):
    """A routed request is not safe to dispatch."""


def _normalized_command(body: str) -> str | None:
    if not isinstance(body, str):
        return None
    return body.replace("\r\n", "\n").replace("\r", "\n").strip()


def parse_command(body: str) -> str | None:
    """Return context only for the original comprehensive exact command."""
    normalized = _normalized_command(body)
    if normalized is None:
        return None
    match = COMMAND_RE.fullmatch(normalized)
    return match.group(1) if match else None


def parse_route(body: str) -> dict[str, str] | None:
    """Resolve an exact GUI command to one of the fixed trusted routes."""
    normalized = _normalized_command(body)
    if normalized is None:
        return None

    comprehensive = COMMAND_RE.fullmatch(normalized)
    if comprehensive:
        validation_kind = "comprehensive"
        execution_context = comprehensive.group(1)
    else:
        date_badge = DATE_BADGE_COMMAND_RE.fullmatch(normalized)
        if date_badge:
            validation_kind = "date-badge"
            execution_context = date_badge.group(1)
        else:
            normal_list = NORMAL_LIST_COMMAND_RE.fullmatch(normalized)
            if normal_list:
                validation_kind = "normal-list"
                execution_context = normal_list.group(1)
            else:
                sidebar_chrome = SIDEBAR_CHROME_COMMAND_RE.fullmatch(normalized)
                if not sidebar_chrome:
                    return None
                validation_kind = "sidebar-chrome"
                execution_context = sidebar_chrome.group(1)

    route = TRUSTED_ROUTES[validation_kind]
    return {
        "validation_kind": validation_kind,
        "execution_context": execution_context,
        "workflow_file": route["workflow_file"],
        "procedure_path": route["procedure_path"],
    }


def trusted_procedure(path: str | Path) -> str:
    """Read PROCEDURE_VERSION from the trusted hosted GUI implementation."""
    tree = ast.parse(Path(path).read_text(encoding="utf-8"))
    values: list[str] = []
    for node in tree.body:
        if not isinstance(node, (ast.Assign, ast.AnnAssign)):
            continue
        targets = node.targets if isinstance(node, ast.Assign) else [node.target]
        value_node = node.value
        for target in targets:
            if isinstance(target, ast.Name) and target.id == "PROCEDURE_VERSION":
                value = ast.literal_eval(value_node)
                if not isinstance(value, str) or not value or "\n" in value or "\r" in value:
                    raise RequestError("PROCEDURE_VERSION must be a non-empty single-line string")
                values.append(value)
    if len(values) != 1:
        raise RequestError("expected exactly one PROCEDURE_VERSION assignment")
    return values[0]


def _require_sha(value: str, name: str) -> str:
    if not isinstance(value, str) or SHA_RE.fullmatch(value) is None:
        raise RequestError(f"{name} is not a lowercase full 40-hex SHA")
    return value


def request_identity(
    pr_number: int,
    head_sha: str,
    base_sha: str,
    execution_context: str,
    procedure: str,
) -> str:
    """Canonical request identity, including the observed current base context."""
    return (
        f"pr={pr_number};head={head_sha};base={base_sha};"
        f"context={execution_context};procedure={procedure}"
    )


def run_name(
    pr_number: int,
    head_sha: str,
    base_sha: str,
    execution_context: str,
    procedure: str,
) -> str:
    """Workflow run title containing the complete routed request identity."""
    return (
        f"{RUN_NAME_PREFIX} PR #{pr_number} @ {head_sha} base {base_sha} "
        f"{execution_context} {procedure}"
    )


def build_request(
    *,
    repository: str,
    pr_number: int,
    actor: str,
    permission: str,
    pr: dict[str, Any],
    base_sha: str,
    execution_context: str,
    procedure: str,
    comment_id: int,
) -> dict[str, str]:
    """Validate current PR facts and build workflow_dispatch inputs."""
    if not repository or "/" not in repository:
        raise RequestError("invalid repository")
    if not isinstance(pr_number, int) or isinstance(pr_number, bool) or pr_number <= 0:
        raise RequestError("pr_number must be a positive integer")
    if not isinstance(comment_id, int) or isinstance(comment_id, bool) or comment_id <= 0:
        raise RequestError("comment_id must be a positive integer")
    if not actor:
        raise RequestError("missing actor")
    if permission not in TRUSTED_PERMISSIONS:
        raise RequestError(f"actor permission is not trusted: {permission or 'none'}")
    if execution_context not in {"head", "merge"}:
        raise RequestError(f"unsupported execution context: {execution_context}")
    if not isinstance(procedure, str) or not procedure or "\n" in procedure or "\r" in procedure:
        raise RequestError("invalid procedure")
    if not isinstance(pr, dict):
        raise RequestError("invalid Pull Request metadata")
    if pr.get("state") != "open" or pr.get("draft") is not False:
        raise RequestError(
            f"target Pull Request must be open and non-draft: state={pr.get('state')} draft={pr.get('draft')}"
        )

    head = pr.get("head") if isinstance(pr.get("head"), dict) else {}
    base = pr.get("base") if isinstance(pr.get("base"), dict) else {}
    head_repo = head.get("repo") if isinstance(head.get("repo"), dict) else {}
    head_repo_name = head_repo.get("full_name", "")
    if head_repo_name != repository:
        raise RequestError(
            f"only same-repository Pull Requests may be dispatched: {head_repo_name or 'unknown'}"
        )

    head_sha = _require_sha(head.get("sha", ""), "current Pull Request head SHA")
    current_base_sha = _require_sha(base_sha, "current target branch SHA")
    if not isinstance(head.get("ref"), str) or not head["ref"]:
        raise RequestError("could not resolve Pull Request head ref")
    if not isinstance(base.get("ref"), str) or not base["ref"]:
        raise RequestError("could not resolve Pull Request base ref")

    return {
        "pr_number": str(pr_number),
        "target_head_sha": head_sha,
        "request_base_sha": current_base_sha,
        "execution_context": execution_context,
        "procedure": procedure,
        "request_comment_id": str(comment_id),
        "request_actor": actor,
        "request_identity": request_identity(
            pr_number, head_sha, current_base_sha, execution_context, procedure
        ),
        "run_name": run_name(
            pr_number, head_sha, current_base_sha, execution_context, procedure
        ),
    }


def find_duplicate(runs: list[dict[str, Any]], expected_run_name: str) -> dict[str, Any] | None:
    """Find an equivalent queued/running/successful request, newest first."""
    if not isinstance(runs, list):
        raise RequestError("workflow_runs must be a list")
    matches = [
        run
        for run in runs
        if isinstance(run, dict) and run.get("display_title") == expected_run_name
    ]
    matches.sort(key=lambda run: run.get("id") if isinstance(run.get("id"), int) else -1, reverse=True)
    for run in matches:
        status = run.get("status")
        conclusion = run.get("conclusion")
        if status != "completed" or conclusion == "success":
            return run
    return None


def _read_json_stdin() -> Any:
    try:
        return json.load(sys.stdin)
    except json.JSONDecodeError as exc:
        raise RequestError(f"invalid JSON input: {exc}") from exc


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("parse")
    sub.add_parser("route")

    procedure_parser = sub.add_parser("procedure")
    procedure_parser.add_argument("path")

    request_parser = sub.add_parser("request")
    request_parser.add_argument("--repository", required=True)
    request_parser.add_argument("--pr-number", required=True, type=int)
    request_parser.add_argument("--actor", required=True)
    request_parser.add_argument("--permission", required=True)
    request_parser.add_argument("--base-sha", required=True)
    request_parser.add_argument("--execution-context", required=True)
    request_parser.add_argument("--procedure", required=True)
    request_parser.add_argument("--comment-id", required=True, type=int)

    duplicate_parser = sub.add_parser("duplicate")
    duplicate_parser.add_argument("--run-name", required=True)

    args = parser.parse_args(argv)
    try:
        if args.command == "parse":
            result = parse_command(sys.stdin.read())
            if result is not None:
                print(result)
            return 0
        if args.command == "route":
            result = parse_route(sys.stdin.read())
            if result is not None:
                print(json.dumps(result, separators=(",", ":"), sort_keys=True))
            return 0
        if args.command == "procedure":
            print(trusted_procedure(args.path))
            return 0
        if args.command == "request":
            request = build_request(
                repository=args.repository,
                pr_number=args.pr_number,
                actor=args.actor,
                permission=args.permission,
                pr=_read_json_stdin(),
                base_sha=args.base_sha,
                execution_context=args.execution_context,
                procedure=args.procedure,
                comment_id=args.comment_id,
            )
            print(json.dumps(request, separators=(",", ":"), sort_keys=True))
            return 0
        if args.command == "duplicate":
            payload = _read_json_stdin()
            runs = payload.get("workflow_runs") if isinstance(payload, dict) else None
            duplicate = find_duplicate(runs, args.run_name)
            print(json.dumps(duplicate, separators=(",", ":"), sort_keys=True) if duplicate else "null")
            return 0
    except (RequestError, OSError, SyntaxError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
