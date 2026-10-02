#!/usr/bin/env python3
"""Stateless request/result guards; never choose an action or mutate a PR."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re

CLASSES = {"initial", "product", "test", "measurement", "environment", "unknown"}
SHA = re.compile(r"[0-9a-f]{40}")
MARKERS = {"implement": "AADW_COMMANDER_HANDOFF_V2", "diagnose": "AADW_DIAGNOSTIC_REQUEST_V1"}


def field(body: str, name: str) -> str:
    values = [line[len(name) + 1:].strip() for line in body.splitlines() if line.startswith(name + ":")]
    if len(values) != 1 or not values[0]:
        raise ValueError(f"{name} must appear exactly once with a value")
    return values[0]


def parse_request(body: str) -> dict:
    action = field(body, "AADW_ACTION")
    if action not in MARKERS or body.splitlines().count(MARKERS[action]) != 1:
        raise ValueError("missing action-specific request marker")
    if MARKERS["diagnose" if action == "implement" else "implement"] in body.splitlines():
        raise ValueError("diagnostic and implementation markers cannot be combined")
    head = field(body, "AADW_TARGET_HEAD").lower()
    if not SHA.fullmatch(head):
        raise ValueError("target head must be a full SHA")
    category = field(body, "AADW_FAILURE_CLASS")
    if category not in CLASSES:
        raise ValueError("unsupported failure class")
    if action == "implement" and category not in {"initial", "product", "test"}:
        raise ValueError("measurement/environment/unknown requires diagnosis, not a product patch")
    root = field(body, "AADW_ROOT_CAUSE")
    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,95}", root):
        raise ValueError("root cause must be a bounded stable identifier")
    evidence = field(body, "AADW_EVIDENCE")
    if len(evidence) > 2048 or not evidence.startswith("https://github.com/"):
        raise ValueError("evidence must reference GitHub facts")
    return {"action": action, "target_sha": head, "failure_class": category,
            "root_cause": root, "evidence": evidence}


def validate_request(body: str, pr: dict, repository: str, history: list, actor: str,
                     comment_id: int) -> dict:
    request = parse_request(body)
    if pr.get("state") != "open":
        raise ValueError("PR is stopped or closed")
    labels = {item.get("name") for item in pr.get("labels", []) if isinstance(item, dict)}
    if "aadw:paused" in labels or "<!-- AADW_PAUSED -->" in (pr.get("body") or ""):
        raise ValueError("PR is paused")
    if pr.get("head", {}).get("repo", {}).get("full_name") != repository:
        raise ValueError("same-repository PR required")
    if pr.get("head", {}).get("sha") != request["target_sha"]:
        raise ValueError("stale target head")
    # Count only previous requests from this already-authorized Commander.
    # This is a conservative duplicate guard, not a model judgment of causality.
    prior = []
    for item in history:
        if item.get("id", comment_id) >= comment_id or item.get("user", {}).get("login") != actor:
            continue
        try:
            candidate = parse_request(item.get("body", ""))
        except ValueError:
            continue
        if candidate["action"] == request["action"] and candidate["root_cause"] == request["root_cause"]:
            prior.append(candidate)
    if request["action"] == "implement":
        if any(item == request for item in prior):
            raise ValueError("duplicate exact-head implementation request; inspect the previous run")
        if sum(item["evidence"] == request["evidence"] for item in prior) >= 2:
            raise ValueError("two requests reused this root cause/evidence; obtain a distinguishing diagnosis")
    request["repository"] = repository
    request["pr_number"] = pr["number"]
    return request


def read_json(path: Path, limit: int = 65536):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > limit:
        raise ValueError("result must be a bounded regular JSON file")
    return json.loads(path.read_text(encoding="utf-8"))


def diagnostic_result(report: dict, request: dict, has_patch: bool) -> dict:
    if request["action"] != "diagnose" or has_patch:
        raise ValueError("diagnosis must not contain a repository patch")
    if not isinstance(report, dict) or report.get("schema_version") != 1:
        raise ValueError("invalid diagnostic schema")
    if report.get("target_sha") != request["target_sha"]:
        raise ValueError("diagnosis describes a different head")
    category = report.get("failure_class")
    if category not in CLASSES - {"initial"}:
        raise ValueError("invalid diagnostic failure class")
    result = {"schema_version": 1, "action": "diagnose", "result": "diagnosis_completed",
              "product_acceptance": "not_evaluated", "target_sha": request["target_sha"],
              "failure_class": category}
    for key in ("summary", "evidence", "next_observation"):
        text = report.get(key)
        if not isinstance(text, str) or not text.strip() or len(text) > 6000:
            raise ValueError(f"missing or oversized diagnostic {key}")
        result[key] = text
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    req = sub.add_parser("request")
    req.add_argument("pr", type=Path)
    req.add_argument("history", type=Path)
    req.add_argument("output", type=Path)
    result = sub.add_parser("diagnosis")
    result.add_argument("request", type=Path)
    result.add_argument("report", type=Path)
    result.add_argument("patch", type=Path)
    result.add_argument("output", type=Path)
    args = parser.parse_args()
    if args.command == "request":
        pages = read_json(args.history, 16 * 1024 * 1024)
        history = [item for page in pages for item in page]
        value = validate_request(os.environ["COMMENT_BODY"], read_json(args.pr, 1024 * 1024),
                                 os.environ["REPOSITORY"], history, os.environ["ACTOR"],
                                 int(os.environ["COMMENT_ID"]))
    else:
        if args.patch.is_symlink() or not args.patch.is_file():
            raise ValueError("patch observation is missing")
        value = diagnostic_result(read_json(args.report), read_json(args.request), args.patch.stat().st_size > 0)
    args.output.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
