"""Close the /implement AADW lifecycle when its source workflow completes.

The normal path correlates through Claude's public progress comment or the AADW
start marker.  Setup failures may happen before either exists, so the completion
path can also correlate the trusted workflow run back to the triggering exact
`/implement` Issue comment using the triggering actor and run creation time.
"""
import datetime as dt
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
_TRIGGER_WINDOW = dt.timedelta(minutes=2)


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


def parse_time(value):
    if not isinstance(value, str) or not value:
        return None
    try:
        return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def issue_from_trigger_comment(comments, actor, run_created_at):
    run_time = parse_time(run_created_at)
    if not actor or run_time is None:
        raise RuntimeError("実装workflowの起動コメント相関情報がありません。")
    candidates = []
    for comment in comments:
        if comment.get("user", {}).get("login") != actor:
            continue
        if (comment.get("body") or "").strip() != "/implement":
            continue
        issue_match = re.search(r"/issues/([1-9][0-9]*)$", comment.get("issue_url") or "")
        created = parse_time(comment.get("created_at"))
        if not issue_match or created is None or created > run_time:
            continue
        delta = run_time - created
        if delta <= _TRIGGER_WINDOW:
            candidates.append((delta, comment.get("id", 0), issue_match.group(1)))
    if not candidates:
        raise RuntimeError("実装workflowに対応する/implementコメントを復元できませんでした。")
    best_delta = min(row[0] for row in candidates)
    closest = [row for row in candidates if row[0] == best_delta]
    issues = {row[2] for row in closest}
    if len(issues) != 1:
        raise RuntimeError("実装workflowに対応する/implementコメントを一意に決定できませんでした。")
    return max(closest, key=lambda row: row[1])[2]


def issue_for_run(call, repository, run_id, attempt, actor=None, run_created_at=None):
    progress = []
    lifecycle = []
    comments = pages(call, f"repos/{repository}/issues/comments?sort=created&direction=desc")
    for comment in comments:
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
    return issue_from_trigger_comment(comments, actor, run_created_at)


def reconcile(call, repository, run_id, attempt, conclusion, *, actor=None, run_created_at=None):
    if not re.fullmatch(r"[1-9][0-9]*", run_id) or not re.fullmatch(r"[1-9][0-9]*", attempt):
        raise ValueError("実装workflowのrun/attemptが不正です。")
    issue = issue_for_run(call, repository, run_id, attempt, actor, run_created_at)
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
        actor = (run.get("triggering_actor") or run.get("actor") or {}).get("login")
        action = reconcile(
            aadw_notify.api,
            repository,
            str(run.get("id") or ""),
            str(run.get("run_attempt") or "1"),
            run.get("conclusion") or "unknown",
            actor=actor,
            run_created_at=run.get("created_at"),
        )
    except Exception as exc:
        print(f"実装完了通知に失敗しました: {exc}", file=sys.stderr)
        return 1
    print(f"実装完了通知: {action}。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
