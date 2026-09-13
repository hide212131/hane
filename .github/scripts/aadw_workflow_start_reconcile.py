"""Create AADW start comments from trusted workflow_run in_progress events.

GitHub does not start another workflow for status events emitted with the
repository GITHUB_TOKEN.  This controller therefore uses the reliable
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
_ACTION_RUN = re.compile(r"/actions/runs/([1-9][0-9]*)(?:[/?#]|$)")


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


def pending_for_source(call, repository, workflow_name, run_id):
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
            if (
                row.get("context") == context
                and row.get("state") == "pending"
                and match
                and match.group(1) == run_id
            ):
                found.append((row.get("id", 0), pr, row))
    return max(found, key=lambda item: item[0], default=None)


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
    if workflow_name not in WORKFLOW_CONTEXT:
        return 0
    if not re.fullmatch(r"[1-9][0-9]*", str(run_id)) or not re.fullmatch(r"[1-9][0-9]*", str(attempt)):
        raise ValueError("source workflowのrun/attemptが不正です。")

    target = None
    for index in range(max(1, poll_attempts)):
        target = pending_for_source(call, repository, workflow_name, str(run_id))
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
