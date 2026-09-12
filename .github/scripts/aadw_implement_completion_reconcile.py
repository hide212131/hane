"""Close the /implement AADW lifecycle when its source workflow completes.

The public start observer runs before Claude Code.  If Claude fails before it
can post the usual Hane progress comment, the start marker is still durable and
contains the exact Issue, run id, and run attempt needed to close the lifecycle.
"""
import json
import os
import re
import sys

import aadw_notify
import aadw_observer

_PROGRESS = re.compile(r"Hane progress run=([1-9][0-9]*) attempt=([1-9][0-9]*)")
_START = re.compile(
    r"process=implement kind=issue number=([1-9][0-9]*) sha=none "
    r"run=([1-9][0-9]*) attempt=([1-9][0-9]*) -->"
)


def pages(call, path):
    rows = []
    for page in range(1, 101):
        current = call(path + ("&" if "?" in path else "?") + f"per_page=100&page={page}")
        if not isinstance(current, list):
            raise RuntimeError("実装完了通知でGitHub APIの一覧を取得できませんでした。")
        rows.extend(current)
        if len(current) < 100:
            return rows
    raise RuntimeError("実装完了通知のページ数が上限を超えました。")


def issue_for_run(call, repository, run_id, attempt):
    progress = []
    lifecycle = []
    for comment in pages(call, f"repos/{repository}/issues/comments?sort=created&direction=desc"):
        body = comment.get("body") or ""
        issue_url = comment.get("issue_url") or ""
        issue_match = re.search(r"/issues/([1-9][0-9]*)$", issue_url)
        if comment.get("user", {}).get("login") == "claude[bot]" and issue_match:
            match = _PROGRESS.search(body)
            if match and match.group(1) == run_id and match.group(2) == attempt:
                progress.append((comment.get("id", 0), issue_match.group(1)))
        if comment.get("user", {}).get("login") == "github-actions[bot]":
            match = _START.search(body)
            if match and match.group(2) == run_id and match.group(3) == attempt:
                lifecycle.append((comment.get("id", 0), match.group(1)))
    if progress:
        return max(progress)[1]
    if lifecycle:
        return max(lifecycle)[1]
    raise RuntimeError("実装workflowに対応するIssueを進捗コメントまたはAADW開始markerから復元できませんでした。")


def reconcile(call, repository, run_id, attempt, conclusion):
    if not re.fullmatch(r"[1-9][0-9]*", run_id) or not re.fullmatch(r"[1-9][0-9]*", attempt):
        raise ValueError("実装workflowのrun/attemptが不正です。")
    issue = issue_for_run(call, repository, run_id, attempt)
    pr = aadw_observer.implementation_pr(call, repository, issue) if conclusion == "success" else None
    if pr:
        state = "success"
        detail = f"実装PR #{pr['number']} を作成しました。"
    else:
        state = "failure"
        detail = (
            "実装workflowは終了しましたが、対応する実装PRを確認できませんでした。"
            if conclusion == "success"
            else f"実装workflowが正常終了しませんでした（{conclusion or 'unknown'}）。"
        )
    return aadw_notify.notify(
        call,
        state=state,
        process="implement",
        kind="issue",
        number=issue,
        sha="none",
        repository=repository,
        run_id=run_id,
        attempt=attempt,
        detail=detail,
    )["action"]


def main():
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    event_path = os.environ.get("GITHUB_EVENT_PATH", "")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        print("実装完了通知: repositoryが不正です。", file=sys.stderr)
        return 1
    try:
        with open(event_path, encoding="utf-8") as stream:
            payload = json.load(stream)
        run = payload.get("workflow_run") or {}
        if (
            payload.get("action") != "completed"
            or run.get("name") != "Implement issue with Claude"
            or run.get("event") != "issue_comment"
        ):
            print("実装完了通知: 対象外イベントです。")
            return 0
        action = reconcile(
            aadw_notify.api,
            repository,
            str(run.get("id") or ""),
            str(run.get("run_attempt") or "1"),
            run.get("conclusion") or "unknown",
        )
    except Exception as exc:
        print(f"実装完了通知に失敗しました: {exc}", file=sys.stderr)
        return 1
    print(f"実装完了通知: {action}。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
