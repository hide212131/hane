"""Reconcile AADW process statuses into human-readable PR Conversation comments.

This controller deliberately reads the durable commit-status history instead of
requiring every producer workflow to duplicate comment-writing logic.  A
`pending` status starts one notification generation; later statuses in the same
context update that generation until another `pending` starts a new one.
"""
import argparse
import datetime as dt
import os
import re
import sys

import aadw_notify

TRUSTED_AUTHORS = None  # owner is added dynamically from repository name
CONTEXTS = {
    "hane/codex-review": "codex-review",
    "hane/copilot-routing": "copilot-pre-gui-routing",
    "hane/claude-fix": "claude-fix",
    "hane/gui-requirement": "gui-requirement",
}
_ACTION_RUN = re.compile(r"/actions/runs/([1-9][0-9]*)(?:/attempts/([1-9][0-9]*))?(?:[/?#]|$)")
_SHORT_SHA = re.compile(r"[0-9a-f]{12}")
_ROUTING_OK = re.compile(
    r"Copilot routing: (?:fix|continue-validation|blocked|workflow changes require owner) for [0-9a-f]{12}$"
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


def run_identity(target_url, fallback_id, seen):
    match = _ACTION_RUN.search(target_url or "")
    if match:
        run_id = match.group(1)
        if match.group(2):
            return run_id, match.group(2)
        seen[run_id] = seen.get(run_id, 0) + 1
        return run_id, str(seen[run_id])
    # Orphan terminal statuses are rare (usually recovery writes). Use the
    # status id only as a stable marker key; the rendered link stays the actual
    # status target URL rather than pretending that this is an Actions run id.
    return str(fallback_id), "1"


def build_generations(statuses):
    """Return notification generations ordered by status id.

    A pending status opens a new generation. Terminal corrections/recovery
    statuses without a new pending update the current generation unless they
    clearly point at a different Actions run.
    """
    generations = []
    current = None
    seen_runs = {}
    for row in sorted(statuses, key=lambda item: item.get("id", 0)):
        state = row.get("state", "")
        parsed = _ACTION_RUN.search(row.get("target_url") or "")
        row_run = parsed.group(1) if parsed else None
        if state == "pending":
            run_id, attempt = run_identity(row.get("target_url"), row.get("id"), seen_runs)
            current = {
                "generation_id": row.get("id"),
                "start": row,
                "latest": row,
                "run_id": run_id,
                "attempt": attempt,
            }
            generations.append(current)
            continue

        if current is None or (row_run and row_run != current["run_id"]):
            run_id, attempt = run_identity(row.get("target_url"), row.get("id"), seen_runs)
            current = {
                "generation_id": row.get("id"),
                "start": None,
                "latest": row,
                "run_id": run_id,
                "attempt": attempt,
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
    return "failure"


def marker(process, pr_number, sha, generation_id):
    return (
        f"<!-- hane-aadw-status: process={process} kind=pr number={pr_number} "
        f"sha={sha} generation={generation_id} -->"
    )


def render(process, state, pr_number, sha, generation, repository, detail):
    label = aadw_notify.PROCESSES[process]
    state_label = aadw_notify.STATE_LABELS[state]
    source = generation.get("start") or generation["latest"]
    run_url = source.get("target_url") or generation["latest"].get("target_url") or ""
    lines = [
        f"### AADW: {label} — {state_label}",
        "",
        f"対象: PR #{pr_number} (SHA `{sha[:12]}`)",
    ]
    if run_url:
        lines.append(f"実行・証跡: {run_url}")
    if detail:
        lines += ["", detail]
    lines += ["", marker(process, pr_number, sha, generation["generation_id"])]
    return "\n".join(lines) + "\n"


def notify_generation(call, repository, pr_number, sha, context, generation, review_source=None):
    process = CONTEXTS[context]
    state = outcome(context, generation["latest"])
    description = generation["latest"].get("description") or "状態の説明はありません。"
    detail = f"状態: {description}"
    if context == "hane/codex-review" and review_source:
        detail += f"\nレビュー実施者: {review_source}"
    marker_text = marker(process, pr_number, sha, generation["generation_id"])
    body = render(process, state, pr_number, sha, generation, repository, detail)
    existing = aadw_notify.find_comment(call, repository, pr_number, marker_text)
    if existing:
        if (existing.get("body") or "") == body:
            return "noop"
        call(f"repos/{repository}/issues/comments/{existing['id']}", {"body": body}, "PATCH")
        return "update"
    call(f"repos/{repository}/issues/{pr_number}/comments", {"body": body}, "POST")
    return "create"


def review_source(statuses):
    rows = [
        row for row in statuses
        if row.get("context") == "hane/review-source"
        and row.get("state") == "success"
        and (row.get("description") or "").startswith("Review source: Copilot fallback for ")
    ]
    return "GitHub Copilot fallback" if rows else None


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
            latest_at = parse_time(generation["latest"].get("created_at"))
            start_at = parse_time((generation.get("start") or {}).get("created_at"))
            if not ((latest_at and latest_at >= cutoff) or (start_at and start_at >= cutoff)):
                continue
            action = notify_generation(
                call, repository, number, sha, context, generation,
                review_source=source if process == "codex-review" else None,
            )
            writes += action != "noop"
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
