"""Pure policy for recovering Claude automatic fixes after a session quota reset."""

import argparse
from datetime import datetime, timedelta, timezone
import json
import re
import sys

SESSION_LIMIT = re.compile(r"you(?:'|’)ve hit your session limit", re.IGNORECASE)
RESET_TIME = re.compile(r"resets\s+(\d{1,2}):(\d{2})\s*(am|pm)\s*\(UTC\)", re.IGNORECASE)
QUOTA_WAIT = re.compile(
    r"Claude fix quota wait until (\d{4}-\d{2}-\d{2}T\d{2}:\d{2}Z) for ([0-9a-f]{12})"
)
MAX_QUOTA_RETRIES = 2


def parse_session_limit(text, now):
    """Return a safe retry time only for an explicit Claude session-limit message."""
    if not SESSION_LIMIT.search(text):
        return {"quota_wait": False, "retry_after": ""}
    match = RESET_TIME.search(text)
    if not match:
        return {"quota_wait": False, "retry_after": ""}

    hour = int(match.group(1))
    minute = int(match.group(2))
    meridiem = match.group(3).lower()
    if not (1 <= hour <= 12 and 0 <= minute <= 59):
        return {"quota_wait": False, "retry_after": ""}
    hour = hour % 12 + (12 if meridiem == "pm" else 0)
    retry = now.astimezone(timezone.utc).replace(hour=hour, minute=minute, second=0, microsecond=0)
    if retry <= now.astimezone(timezone.utc):
        retry += timedelta(days=1)
    # Do not stampede Claude exactly on the advertised reset boundary.
    retry += timedelta(minutes=2)
    return {"quota_wait": True, "retry_after": retry.strftime("%Y-%m-%dT%H:%MZ")}


def parse_wait(description, sha):
    match = QUOTA_WAIT.fullmatch(description or "")
    if not match or match.group(2) != sha[:12]:
        return None
    try:
        return datetime.strptime(match.group(1), "%Y-%m-%dT%H:%MZ").replace(tzinfo=timezone.utc)
    except ValueError:
        return None


def quota_wait_count(statuses, sha):
    short = sha[:12]
    return sum(
        1 for row in statuses
        if row.get("context") == "hane/claude-fix"
        and QUOTA_WAIT.fullmatch(row.get("description", ""))
        and row.get("description", "").endswith(f" for {short}")
    )


def recovery(statuses, sha, now):
    """Decide whether a recorded quota wait may be re-dispatched."""
    rows = [s for s in statuses if s.get("context") == "hane/claude-fix"]
    latest = max(rows, key=lambda s: s.get("id", -1), default={})
    retry_after = parse_wait(latest.get("description", ""), sha)
    count = quota_wait_count(statuses, sha)
    result = {
        "recover": False,
        "retry_after": retry_after.strftime("%Y-%m-%dT%H:%MZ") if retry_after else "",
        "quota_wait_count": count,
        "retry_number": count,
    }
    if retry_after is None or now.astimezone(timezone.utc) < retry_after:
        return result
    # wait #1 permits retry #1; wait #2 permits retry #2; a third quota hit is terminal.
    result["recover"] = 1 <= count <= MAX_QUOTA_RETRIES
    return result


def parse_now(value):
    if not value:
        return datetime.now(timezone.utc)
    return datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(timezone.utc)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("parse-log", "recovery"))
    parser.add_argument("--sha", default="")
    parser.add_argument("--now", default="")
    args = parser.parse_args()
    now = parse_now(args.now)
    if args.mode == "parse-log":
        result = parse_session_limit(sys.stdin.read(), now)
    else:
        if not args.sha:
            parser.error("--sha is required for recovery")
        result = recovery(json.load(sys.stdin), args.sha, now)
    json.dump(result, sys.stdout)
    print()


if __name__ == "__main__":
    main()
