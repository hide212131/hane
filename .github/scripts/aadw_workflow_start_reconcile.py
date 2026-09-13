"""Create AADW start comments from trusted workflow_run in_progress events.

GitHub does not start another workflow for status events emitted with the
repository GITHUB_TOKEN. This controller therefore uses the reliable
workflow_run event and correlates the source run to its pending commit status.
The workflow_run payload supplies the authoritative run attempt.
"""
import argparse
import os
import re
import sys
import time

import aadw_notification_reconcile as status_reconcile
import aadw_notify

WORKFLOW_CONTEXT = {
    "Codex review gate": "hane/codex-review",
    "Copilot pre-GUI routing": "hane/copilot-routing",
    "Copilot routing reconciliation": "hane/copilot-routing",
    "Claude automatic fix worker": "hane/claude-fix",
    "GUI requirement classification": "hane/gui-requirement",
    "GUI validation": "hane/gui-validation",
    "Copilot final judgement": "hane/final-judge",
}
_FALLBACK_WORKFLOW = "Codex limit Copilot review fallback"
_ACTION_RUN = re.compile(r"/actions/runs/([1-9][0-9]*)(?:[/?#]|$)")
_FALLBACK_MARKER = re.compile(
    r"process=codex-review-fallback kind=pr number=([1-9][0-9]*) sha=([0-9a-f]{40}) "
    r"run=([1-9][0-9]*) attempt=([1-9][0-9]*) -->"
)


def pages(call, path):
    rows = []
    for page in range(1, 101):
        current = call(path + ("&" if "?" in path else "?") + f"per_page=100&page={page}")
        if not isinstance(current, list):
            raise RuntimeError("AADW開始通知でGitHub APIの一覧を取得できませんでした。")
        rows.extend(current)
        if len(current) < 100:
            return rows
    raise RuntimeError("AADW開始通知のページ数が上限を超えました。")


def pending_for_source(call, repository, workflow_name, run_id, started_at):
    context = WORKFLOW_CONTEXT.get(workflow_name)
    if context is None:
        return None
    found = []
    for pr in pages(call, f"repos/{repository}/pulls?state=open"):
        if not status_reconcile.trusted_pr(pr, repository):
            continue
        sha = pr.get("head", {}).get("sha") or ""
        for row in pages(call, f"repos/{repository}/commits/{sha}/statuses"):
            match = _ACTION_RUN.search(row.get("target_url") or "")
            created_at = status_reconcile.parse_time(row.get("created_at"))
            if (
                row.get("context") == context
                and row.get("state") == "pending"
                and match
                and match.group(1) == run_id
                and created_at is not None
                and created_at >= started_at
            ):
                found.append((row.get("id", 0), pr, row))
    return max(found, key=lambda item: item[0], default=None)


def trusted_rerun_pr(pr, repository):
    owner = repository.split("/", 1)[0]
    return (
        pr.get("draft") is False
        and (pr.get("head", {}).get("repo") or {}).get("full_name") == repository
        and pr.get("user", {}).get("login") in (owner, "github-actions[bot]", "claude[bot]")
        and re.fullmatch(r"[0-9a-f]{40}", pr.get("head", {}).get("sha", "")) is not None
    )


def fallback_rerun_target(call, repository, run_id, attempt):
    """Recover the PR for fallback reruns from an earlier attempt marker."""
    current_attempt = int(attempt)
    if current_attempt <= 1:
        return None
    found = []
    for comment in pages(call, f"repos/{repository}/issues/comments?sort=created&direction=desc"):
        if comment.get("user", {}).get("login") != "github-actions[bot]":
            continue
        match = _FALLBACK_MARKER.search(comment.get("body") or "")
        if not match:
            continue
        number, _old_sha, marker_run, marker_attempt = match.groups()
        prior_attempt = int(marker_attempt)
        if marker_run == run_id and prior_attempt < current_attempt:
            found.append((prior_attempt, comment.get("id", 0), number))
    if not found:
        return None
    _, _, number = max(found, key=lambda row: (row[0], row[1]))
    pr = call(f"repos/{repository}/pulls/{number}")
    if not trusted_rerun_pr(pr, repository):
        raise RuntimeError("fallback rerunの相関markerが信頼できないPRを指しています。")
    return pr


def reconcile_fallback_rerun_start(call, repository, run_id, attempt):
    if int(attempt) <= 1:
        # attempt 1 is created by the external Codex-limit issue_comment observer.
        return 0
    pr = fallback_rerun_target(call, repository, run_id, attempt)
    if not pr:
        raise RuntimeError("fallback rerunの前attempt相関を復元できませんでした。")
    result = aadw_notify.notify(
        call,
        state="start",
        process="codex-review-fallback",
        kind="pr",
        number=pr["number"],
        sha=pr["head"]["sha"],
        repository=repository,
        run_id=str(run_id),
        attempt=str(attempt),
        detail=f"Copilot代替レビューのrerun attempt {attempt} を開始しました。",
    )
    return int(result["action"] != "noop")


def reconcile_start(
    call,
    repository,
    workflow_name,
    run_id,
    attempt,
    *,
    poll_attempts=1,
    poll_seconds=0,
    sleeper=time.sleep,
):
    if not re.fullmatch(r"[1-9][0-9]*", str(run_id)) or not re.fullmatch(r"[1-9][0-9]*", str(attempt)):
        raise ValueError("source workflowのrun/attemptが不正です。")
    if workflow_name == _FALLBACK_WORKFLOW:
        return reconcile_fallback_rerun_start(call, repository, str(run_id), str(attempt))
    if workflow_name not in WORKFLOW_CONTEXT:
        return 0

    started_at = status_reconcile.attempt_started_at(
        call, repository, str(run_id), str(attempt)
    )
    target = None
    for index in range(max(1, poll_attempts)):
        target = pending_for_source(
            call, repository, workflow_name, str(run_id), started_at
        )
        if target:
            break
        if index + 1 < max(1, poll_attempts) and poll_seconds > 0:
            sleeper(poll_seconds)
    if not target:
        return 0

    _, pr, status = target
    sha = pr["head"]["sha"]
    context = WORKFLOW_CONTEXT[workflow_name]
    process = status_reconcile.CONTEXTS[context]
    result = aadw_notify.notify(
        call,
        state="start",
        process=process,
        kind="pr",
        number=pr["number"],
        sha=sha,
        repository=repository,
        run_id=str(run_id),
        attempt=str(attempt),
        detail=f"状態: {status.get('description') or '処理を開始しました。'}",
    )
    return int(result["action"] != "noop")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--poll-attempts", type=int, default=15)
    parser.add_argument("--poll-seconds", type=float, default=2.0)
    args = parser.parse_args()
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    workflow_name = os.environ.get("AADW_SOURCE_WORKFLOW", "")
    run_id = os.environ.get("AADW_SOURCE_RUN_ID", "")
    attempt = os.environ.get("AADW_SOURCE_RUN_ATTEMPT", "")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        print("AADW開始通知: repositoryが不正です。", file=sys.stderr)
        return 1
    try:
        writes = reconcile_start(
            aadw_notify.api,
            repository,
            workflow_name,
            run_id,
            attempt,
            poll_attempts=max(1, args.poll_attempts),
            poll_seconds=max(0.0, args.poll_seconds),
        )
    except Exception as exc:
        print(f"AADW開始通知に失敗しました: {exc}", file=sys.stderr)
        return 1
    print(f"AADW開始通知: {writes}件を作成または更新しました。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
