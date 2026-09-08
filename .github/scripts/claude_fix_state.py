"""Pure decisions shared by the trusted Claude worker and reconciler.

Commit statuses are an event log, not a single mutable PR state. Reduce each
authorization context independently. A successful authorization permits ONE
paid invocation; a pending claim is deliberately not automatically retried.
"""

import argparse
from datetime import datetime, timezone
import json
import re
import sys

MANUAL = re.compile(r"hane/claude-fix-manual/[0-9]+-[0-9]+")
FIX_SUBJECT = re.compile(r"Automatic Claude fix for Copilot routing decision on ([0-9a-f]{12})")


def latest_by_context(statuses):
    latest = {}
    for status in statuses:
        context = status.get("context", "")
        if status["id"] > latest.get(context, {}).get("id", -1):
            latest[context] = status
    return latest


def authorizations(statuses, sha):
    expected = f"Claude manual retry authorized for {sha[:12]}"
    return sorted(
        (s for context, s in latest_by_context(statuses).items()
         if MANUAL.fullmatch(context) and s.get("state") == "success"
         and s.get("description") == expected),
        key=lambda s: s["id"],
    )


def authorization_valid(statuses, sha, context):
    return any(s["context"] == context for s in authorizations(statuses, sha))


def age_seconds(status, now):
    try:
        created = datetime.fromisoformat(status["created_at"].replace("Z", "+00:00"))
        return (now - created).total_seconds()
    except (KeyError, ValueError, TypeError):
        # Unknown age must not permit stealing a potentially live execution.
        return 0


def recovery(statuses, sha, now):
    short = sha[:12]
    latest = latest_by_context(statuses)
    manual = authorizations(statuses, sha)
    route = latest.get("hane/copilot-routing", {}).get("description", "")
    fix = f"Copilot routing: fix for {short}"
    owner = f"Copilot routing: workflow changes require owner for {short}"
    failed = f"Copilot routing controller failed for {short}"
    fix_id = max((s["id"] for s in statuses if s.get("context") == "hane/copilot-routing"
                  and s.get("description") == fix and s.get("state") == "failure"), default=0)
    owner_id = max((s["id"] for s in statuses if s.get("context") == "hane/copilot-routing"
                    and s.get("description") == owner), default=0)
    route_ok = (route == fix or (route == failed and fix_id > owner_id)
                or (bool(manual) and fix_id > 0))
    result = {"recover": False, "manual_retry": False, "manual_retry_context": ""}
    if not route_ok:
        return result
    execution = latest.get("hane/claude-fix", {})
    description = execution.get("description", "")
    age = age_seconds(execution, now)
    if description in (f"Claude fix pending for {short}", f"Claude fix running for {short}"):
        if age < 3300:
            return result
    elif description == f"Claude fix controller failed for {short}" and age < 60:
        return result

    # A new command is independent of an older failed/no-change/handoff result.
    # Once claimed, this authorization is absent from `manual`, even if a
    # terminal status POST was lost or a stale dispatch was already queued.
    if manual:
        return {"recover": True, "manual_retry": True,
                "manual_retry_context": manual[0]["context"]}

    # Never turn an uncertain manual execution into an automatic paid retry.
    # A new explicit command is required after the claim boundary.
    if any(MANUAL.fullmatch(c) for c in latest):
        return result
    recoverable = ("", f"Claude fix pending for {short}", f"Claude fix running for {short}",
                   f"Claude fix controller failed for {short}")
    result["recover"] = description in recoverable
    return result


def cycle_budget(commits, statuses_by_sha, target_sha):
    indices = {c["sha"]: i for i, c in enumerate(commits)}
    boundary = -1
    completed = set()
    for commit in commits:
        sha = commit["sha"]
        index = indices[sha]
        statuses = statuses_by_sha.get(sha, [])
        # Old cycle markers remain compatible; their publication order is irrelevant.
        if any(s.get("context") == "hane/claude-fix-cycle" and s.get("state") == "success"
               and s.get("description") == f"Claude fix cycle reset for {sha[:12]} after manual retry"
               for s in statuses):
            boundary = index
        if any(s.get("context") == "hane/claude-fix" and s.get("state") == "success"
               and s.get("description") == f"Claude fix completed for {sha[:12]}" for s in statuses):
            completed.add(sha)
        message = commit.get("commit", {}).get("message", "")
        subject = message.split("\n", 1)[0]
        actor = (commit.get("author") or {}).get("login") or (commit.get("committer") or {}).get("login")
        match = FIX_SUBJECT.fullmatch(subject)
        parents = commit.get("parents", [])
        if actor != "github-actions[bot]" or not match or len(parents) != 1:
            continue
        predecessor = parents[0]["sha"]
        if predecessor not in indices or not predecessor.startswith(match[1]):
            continue
        # A pushed fix counts even if publishing its completion status failed.
        completed.add(predecessor)
        if "Manual retry: true" in message.splitlines():
            boundary = index
    count = sum(indices[s] >= boundary for s in completed if s != target_sha)
    return {"completed": count, "boundary_sha": commits[boundary]["sha"] if boundary >= 0 else ""}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("authorization", "recovery", "budget"))
    parser.add_argument("--sha", required=True)
    parser.add_argument("--context", default="")
    args = parser.parse_args()
    data = json.load(sys.stdin)
    if args.mode == "authorization":
        result = {"valid": authorization_valid(data, args.sha, args.context),
                  "active": bool(authorizations(data, args.sha))}
    elif args.mode == "recovery":
        result = recovery(data, args.sha, datetime.now(timezone.utc))
    else:
        result = cycle_budget(data["commits"], data["statuses"], args.sha)
    json.dump(result, sys.stdout)
    print()


if __name__ == "__main__":
    main()
