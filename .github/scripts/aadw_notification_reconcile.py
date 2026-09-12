"""Reconcile durable AADW commit statuses into Issue/PR lifecycle comments.

Producer workflows keep commit statuses as the machine-readable source of truth.
This controller mirrors those histories into Conversation comments so every
user-visible process exposes start/normal-end/abnormal-end without duplicating
comment logic in each producer workflow.
"""
import argparse
import datetime as dt
import os
import re
import sys

import aadw_notify

CONTEXTS = {
    "hane/codex-review": "codex-review",
    "hane/copilot-routing": "copilot-pre-gui-routing",
    "hane/claude-fix": "claude-fix",
    "hane/gui-requirement": "gui-requirement",
    "hane/gui-validation": "gui-validation",
    "hane/final-judge": "final-judge",
}
_ACTION_RUN = re.compile(r"/actions/runs/([1-9][0-9]*)(?:/attempts/([1-9][0-9]*))?(?:[/?#]|$)")
_GUI_GENERATION = re.compile(r" g([1-9][0-9]*)-([1-9][0-9]*)$")
_ROUTING_OK = re.compile(
    r"Copilot routing: (?:fix|continue-validation|blocked|workflow changes require owner) for [0-9a-f]{12}$"
)
_GUI_NORMAL = re.compile(r"GUI (?:pass|fail) v[0-9]+ [0-9a-f]{12} g[1-9][0-9]*-[1-9][0-9]*$")
_FINAL_NORMAL = re.compile(r"Final (?:ready|merged|fix|blocked) v[0-9]+ [0-9a-f]{12} e[0-9a-f]+$")
_FALLBACK_MARKER = re.compile(
    r"process=codex-review-fallback kind=pr number=([1-9][0-9]*) sha=([0-9a-f]{40}) "
    r"run=([1-9][0-9]*) attempt=([1-9][0-9]*) -->"
)


def pages(call, path):
    rows = []
    for page in range(1, 101):
        current = call(path + ("&" if "?" in path else "?") + f"per_page=100&page={page}")
        if not isinstance(current, list):
            raise RuntimeError("AADW通知reconcileでGitHub APIの一覧を取得できませんでした。")
        rows.extend(current)
        if len(current) < 100:
            return rows
    raise RuntimeError("AADW通知reconcileのページ数が上限を超えました。")


def parse_time(value):
    if not isinstance(value, str) or not value:
        return None
    try:
        return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def run_identity(status, fallback_id, seen):
    description = status.get("description") or ""
    gui = _GUI_GENERATION.search(description)
    if gui:
        return gui.group(1), gui.group(2)
    match = _ACTION_RUN.search(status.get("target_url") or "")
    if match:
        run_id = match.group(1)
        if match.group(2):
            return run_id, match.group(2)
        seen[run_id] = seen.get(run_id, 0) + 1
        return run_id, str(seen[run_id])
    return str(fallback_id), "1"


def build_generations(statuses):
    """Group one context's append-only status history into process runs."""
    generations = []
    current = None
    seen_runs = {}
    for row in sorted(statuses, key=lambda item: item.get("id", 0)):
        state = row.get("state", "")
        row_run, row_attempt = run_identity(row, row.get("id"), seen_runs)
        if state == "pending":
            current = {
                "generation_id": row.get("id"),
                "start": row,
                "latest": row,
                "run_id": row_run,
                "attempt": row_attempt,
            }
            generations.append(current)
            continue
        if current is None or row_run != current["run_id"] or row_attempt != current["attempt"]:
            current = {
                "generation_id": row.get("id"),
                "start": None,
                "latest": row,
                "run_id": row_run,
                "attempt": row_attempt,
            }
            generations.append(current)
        else:
            current["latest"] = row
    return generations


def outcome(context, status):
    state = status.get("state", "")
    description = status.get("description") or ""
    if state == "pending":
        return "start"
    if context in ("hane/codex-review", "hane/gui-requirement"):
        return "success" if state in ("success", "failure") else "failure"
    if context == "hane/copilot-routing":
        return "success" if _ROUTING_OK.fullmatch(description) else "failure"
    if context == "hane/claude-fix":
        return "success" if state == "success" and description.startswith("Claude fix completed for ") else "failure"
    if context == "hane/gui-validation":
        return "success" if _GUI_NORMAL.fullmatch(description) else "failure"
    if context == "hane/final-judge":
        return "success" if _FINAL_NORMAL.fullmatch(description) else "failure"
    return "failure"


def recent(generation, cutoff):
    latest_at = parse_time(generation["latest"].get("created_at"))
    start_at = parse_time((generation.get("start") or {}).get("created_at"))
    return bool((latest_at and latest_at >= cutoff) or (start_at and start_at >= cutoff))


def notify_generation(call, repository, pr_number, sha, context, generation, source=None):
    process = CONTEXTS[context]
    state = outcome(context, generation["latest"])
    description = generation["latest"].get("description") or "状態の説明はありません。"
    detail = f"状態: {description}"
    if context == "hane/codex-review" and source:
        detail += f"\nレビュー実施者: {source}"
    return aadw_notify.notify(
        call,
        state=state,
        process=process,
        kind="pr",
        number=pr_number,
        sha=sha,
        repository=repository,
        run_id=generation["run_id"],
        attempt=generation["attempt"],
        detail=detail,
    )["action"]


def review_source(statuses):
    rows = [
        row for row in statuses
        if row.get("context") == "hane/review-source"
        and row.get("state") == "success"
        and (row.get("description") or "").startswith("Review source: Copilot fallback for ")
    ]
    return "GitHub Copilot fallback" if rows else None


def active_fallback_run(call, repository, pr_number, sha):
    found = []
    for comment in pages(call, f"repos/{repository}/issues/{pr_number}/comments"):
        if comment.get("user", {}).get("login") != "github-actions[bot]":
            continue
        body = comment.get("body") or ""
        match = _FALLBACK_MARKER.search(body)
        if (
            match
            and match.group(1) == str(pr_number)
            and match.group(2) == sha
            and "— 処理開始" in body
        ):
            found.append((comment.get("id", 0), match.group(3), match.group(4)))
    return max(found)[1:] if found else None


def reconcile_fallback(call, repository, pr_number, sha, statuses, cutoff):
    rows = [
        row for row in statuses
        if row.get("context") == "hane/review-source"
        and parse_time(row.get("created_at"))
        and parse_time(row.get("created_at")) >= cutoff
    ]
    if not rows:
        return "noop"
    latest = max(rows, key=lambda row: row.get("id", 0))
    correlation = active_fallback_run(call, repository, pr_number, sha)
    if not correlation:
        return "noop"
    state = (
        "success"
        if latest.get("state") == "success"
        and (latest.get("description") or "").startswith("Review source: Copilot fallback for ")
        else "failure"
    )
    detail = (
        "Copilotによる代替レビューが完了しました。"
        if state == "success"
        else "Copilotによる代替レビューを正常に完了できませんでした。"
    )
    return aadw_notify.notify(
        call,
        state=state,
        process="codex-review-fallback",
        kind="pr",
        number=pr_number,
        sha=sha,
        repository=repository,
        run_id=correlation[0],
        attempt=correlation[1],
        detail=detail,
    )["action"]


def trusted_pr(pr, repository):
    owner = repository.split("/", 1)[0]
    return (
        pr.get("state") == "open"
        and pr.get("draft") is False
        and (pr.get("head", {}).get("repo") or {}).get("full_name") == repository
        and pr.get("user", {}).get("login") in (owner, "github-actions[bot]", "claude[bot]")
        and re.fullmatch(r"[0-9a-f]{40}", pr.get("head", {}).get("sha", "")) is not None
    )


def reconcile_pr(call, repository, pr, cutoff):
    if not trusted_pr(pr, repository):
        return 0
    number = int(pr["number"])
    sha = pr["head"]["sha"]
    statuses = pages(call, f"repos/{repository}/commits/{sha}/statuses")
    source = review_source(statuses)
    writes = 0
    for context, process in CONTEXTS.items():
        rows = [row for row in statuses if row.get("context") == context]
        for generation in build_generations(rows):
            if not recent(generation, cutoff):
                continue
            action = notify_generation(
                call, repository, number, sha, context, generation,
                source=source if process == "codex-review" else None,
            )
            writes += action != "noop"
    writes += reconcile_fallback(call, repository, number, sha, statuses, cutoff) != "noop"
    return writes


def reconcile(call, repository, *, lookback_seconds=7200, now=None):
    now = now or dt.datetime.now(dt.timezone.utc)
    cutoff = now - dt.timedelta(seconds=lookback_seconds)
    pulls = pages(call, f"repos/{repository}/pulls?state=open")
    writes = 0
    for pr in pulls:
        writes += reconcile_pr(call, repository, pr, cutoff)
    return writes


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--lookback-seconds", type=int, default=7200)
    args = parser.parse_args()
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        print("AADW通知reconcile: repositoryが不正です。", file=sys.stderr)
        return 1
    try:
        writes = reconcile(aadw_notify.api, repository, lookback_seconds=max(300, args.lookback_seconds))
    except Exception as exc:
        print(f"AADW通知reconcileに失敗しました: {exc}", file=sys.stderr)
        return 1
    print(f"AADW通知reconcile: {writes}件を作成または更新しました。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
