"""Close the /implement AADW lifecycle when its source workflow completes.

The normal path correlates through Claude's public progress comment or the AADW
start marker. Setup failures may happen before either exists, so the completion
path also accepts the workflow run's trusted display title, which embeds the
exact Issue number via implement.yml's run-name.
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
_RUN_TITLE = re.compile(r"Implement issue #([1-9][0-9]*)")


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


def issue_from_run_title(display_title):
    match = _RUN_TITLE.fullmatch(display_title or "")
    if not match:
        raise RuntimeError("実装workflowのrun-nameからIssue番号を復元できませんでした。")
    return match.group(1)


def issue_for_run(call, repository, run_id, attempt, display_title=None):
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
    return issue_from_run_title(display_title)


def reconcile(call, repository, run_id, attempt, conclusion, *, display_title=None):
    if not re.fullmatch(r"[1-9][0-9]*", run_id) or not re.fullmatch(r"[1-9][0-9]*", attempt):
        raise ValueError("実装workflowのrun/attemptが不正です。")
    issue = issue_for_run(call, repository, run_id, attempt, display_title)
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
            display_title=run.get("display_title"),
        )
    except Exception as exc:
        print(f"実装完了通知に失敗しました: {exc}", file=sys.stderr)
        return 1
    print(f"実装完了通知: {action}。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
