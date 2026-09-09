"""Pure decisions shared by the trusted Claude worker and reconciler.

Commit statuses are an event log, not a single mutable PR state. Reduce each
authorization context independently. A successful authorization permits ONE
paid invocation; a pending claim is deliberately not automatically retried.
"""

import argparse
from datetime import datetime, timezone
import json
import os
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


def routing_allows_fix(statuses, sha, manual=False):
    """Ignore transport failures, never superseding routing decisions.

    An explicit owner grant may bypass an owner-only workflow guard, but not
    a newer continue/blocked/pending/unknown decision. Inspect the actual
    preceding event instead of resurrecting an arbitrary historical fix.
    """
    short = sha[:12]
    ignored = {f"Copilot routing controller failed for {short}"}
    if manual:
        ignored.add(f"Copilot routing: workflow changes require owner for {short}")
    for event in sorted((s for s in statuses if s.get("context") in ("hane/copilot-routing", "hane/final-judge")),
                        key=lambda s: s["id"], reverse=True):
        if event.get("context") == "hane/final-judge":
            return (event.get("state") == "failure" and re.fullmatch(
                rf"Final fix v1 {short} e[0-9a-f]{{16}}", event.get("description", "")) is not None)
        if event.get("state") == "error" and event.get("description") in ignored:
            continue
        return (event.get("state") == "failure"
                and event.get("description") == f"Copilot routing: fix for {short}")
    return False


def manual_command_allows_fix(statuses, sha, context, now):
    """Let one explicit owner command supersede only decisions older than it.

    The worker calls this after validating the exact manual authorization
    context. A routing/final decision created after that authorization still
    wins, so a late no-fix decision can stop the paid invocation. A previously
    active execution lease is also not stolen by a newer owner command, unless
    that lease has already expired under the same 3300-second budget the
    worker itself uses to reclaim a stale pending/running lease.
    """
    if not MANUAL.fullmatch(context):
        return False

    short = sha[:12]
    authorized = f"Claude manual retry authorized for {short}"
    claimed = f"Claude manual retry claimed for {short}"
    grants = [s for s in statuses
              if s.get("context") == context
              and s.get("state") == "success"
              and s.get("description") == authorized]
    if not grants:
        return False
    grant = max(grants, key=lambda s: s["id"])

    current = latest_by_context(statuses).get(context, {})
    if not ((current.get("state") == "success" and current.get("description") == authorized)
            or (current.get("state") == "pending" and current.get("description") == claimed)):
        return False

    execution = latest_by_context(statuses).get("hane/claude-fix", {})
    if (execution.get("id", -1) < grant["id"]
            and execution.get("description") in (
                f"Claude fix pending for {short}", f"Claude fix running for {short}")
            and age_seconds(execution, now) < 3300):
        return False

    ignored = {
        f"Copilot routing controller failed for {short}",
        f"Copilot routing: workflow changes require owner for {short}",
    }
    events = sorted(
        (s for s in statuses
         if s.get("id", -1) > grant["id"]
         and s.get("context") in ("hane/copilot-routing", "hane/final-judge")),
        key=lambda s: s["id"], reverse=True,
    )
    for event in events:
        if event.get("context") == "hane/final-judge":
            return (event.get("state") == "failure" and re.fullmatch(
                rf"Final fix v1 {short} e[0-9a-f]{{16}}", event.get("description", "")) is not None)
        if event.get("state") == "error" and event.get("description") in ignored:
            continue
        return (event.get("state") == "failure"
                and event.get("description") == f"Copilot routing: fix for {short}")
    # Nothing happened since the grant: trust it, but only once some routing
    # or final-judge evidence has ever been recorded for this sha.
    return any(s.get("context") in ("hane/copilot-routing", "hane/final-judge") for s in statuses)


def recovery(statuses, sha, now):
    short = sha[:12]
    latest = latest_by_context(statuses)
    manual = authorizations(statuses, sha)
    result = {"recover": False, "manual_retry": False, "manual_retry_context": ""}
    # An active grant is context-aware: it must only be checked against
    # decisions recorded after it, the same rule the worker applies when a
    # lost repository_dispatch POST otherwise strands a persisted approval.
    # Several grants can be outstanding at once (e.g. an older one whose
    # dispatch failed, superseded by a newer no-fix decision, followed by a
    # fresh grant); search from the newest so a still-permitted approval is
    # never shadowed by an older one a later routing decision blocked.
    chosen = None
    if manual:
        for candidate in reversed(manual):
            if manual_command_allows_fix(statuses, sha, candidate["context"], now):
                chosen = candidate
                break
        if chosen is None:
            return result
    elif not routing_allows_fix(statuses, sha, False):
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
                "manual_retry_context": chosen["context"]}

    # Never turn an uncertain manual execution into an automatic paid retry.
    # A new explicit command is required after the claim boundary.
    if any(MANUAL.fullmatch(c) for c in latest):
        return result
    recoverable = ("", f"Claude fix not started for {short}", f"Claude fix pending for {short}", f"Claude fix running for {short}",
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
    parser.add_argument("mode", choices=("authorization", "recovery", "budget", "routing"))
    parser.add_argument("--sha", required=True)
    parser.add_argument("--context", default="")
    parser.add_argument("--manual", choices=("true", "false"), default="false")
    args = parser.parse_args()
    data = json.load(sys.stdin)
    if args.mode == "authorization":
        result = {"valid": authorization_valid(data, args.sha, args.context),
                  "active": bool(authorizations(data, args.sha))}
    elif args.mode == "routing":
        manual = args.manual == "true"
        manual_context = args.context or os.environ.get("MANUAL_RETRY_CONTEXT", "")
        if manual and manual_context:
            result = {"allowed": manual_command_allows_fix(
                data, args.sha, manual_context, datetime.now(timezone.utc))}
        else:
            result = {"allowed": routing_allows_fix(data, args.sha, manual)}
    elif args.mode == "recovery":
        result = recovery(data, args.sha, datetime.now(timezone.utc))
    else:
        result = cycle_budget(data["commits"], data["statuses"], args.sha)
    json.dump(result, sys.stdout)
    print()


if __name__ == "__main__":
    main()
