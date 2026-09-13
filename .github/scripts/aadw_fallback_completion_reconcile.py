"""Close an active Copilot fallback lifecycle when its workflow completes."""
import json
import os
import re
import sys

import aadw_notify
import aadw_observer

_FALLBACK_START = re.compile(
    r'process=codex-review-fallback kind=pr number=([1-9][0-9]*) '
    r'sha=([0-9a-f]{40}) run=([1-9][0-9]*) attempt=([1-9][0-9]*) -->'
)


def fallback_target(call, repository, run_id, attempt):
    """Resolve the exact fallback start even after its PR has been closed."""
    found = []
    for comment in aadw_observer.pages(
        call, f'repos/{repository}/issues/comments?sort=created&direction=desc'
    ):
        if comment.get('user', {}).get('login') != 'github-actions[bot]':
            continue
        body = comment.get('body') or ''
        if '— 処理開始' not in body:
            continue
        match = _FALLBACK_START.search(body)
        if not match:
            continue
        number, sha, marker_run, marker_attempt = match.groups()
        if marker_run != run_id or marker_attempt != attempt:
            continue
        pr = call(f'repos/{repository}/pulls/{number}')
        if not aadw_observer.trusted_pr(pr, repository):
            raise RuntimeError('fallback開始markerが信頼できないPRを指しています。')
        found.append((comment.get('id', 0), pr, sha))
    return max(found, key=lambda row: row[0], default=None)


def reconcile(call, repository, payload):
    if payload.get('action') != 'completed':
        return 0
    run = payload.get('workflow_run') or {}
    if run.get('name') != 'Codex limit Copilot review fallback':
        return 0
    run_id = str(run.get('id') or '')
    attempt = str(run.get('run_attempt') or '1')
    target = fallback_target(call, repository, run_id, attempt)
    if not target:
        return 0
    comment_id, pr, sha = target
    start_comment = call(f'repos/{repository}/issues/comments/{comment_id}')
    start_created_at = start_comment.get('created_at') if isinstance(start_comment, dict) else None
    if not isinstance(start_created_at, str) or not start_created_at:
        raise RuntimeError('fallback開始コメントの作成時刻を取得できませんでした。')

    statuses = aadw_observer.pages(call, f'repos/{repository}/commits/{sha}/statuses')
    source = max(
        (
            row for row in statuses
            if row.get('context') == 'hane/review-source'
            and isinstance(row.get('created_at'), str)
            and row.get('created_at') >= start_created_at
        ),
        key=lambda row: row.get('id', -1), default=None,
    )
    completed = bool(
        source
        and source.get('state') == 'success'
        and (source.get('description') or '').startswith('Review source: Copilot fallback for ')
    )
    if completed:
        state = 'success'
        detail = 'Copilotによる代替レビューが完了しました。'
    else:
        state = 'failure'
        if run.get('conclusion') == 'success':
            detail = 'fallback workflowは終了しましたが、現在run開始後のexact-head代替レビュー終端証跡がありません。head変更またはstale終了として扱います。'
        else:
            detail = f'Copilot代替レビューworkflowが正常終了しませんでした（{run.get("conclusion") or "unknown"}）。'
    result = aadw_notify.notify(
        call,
        state=state, process='codex-review-fallback', kind='pr', number=pr['number'],
        sha=sha, repository=repository, run_id=run_id, attempt=attempt, detail=detail,
    )
    return result['action'] != 'noop'


def main():
    repository = os.environ.get('GITHUB_REPOSITORY', '')
    try:
        with open(os.environ['GITHUB_EVENT_PATH'], encoding='utf-8') as stream:
            payload = json.load(stream)
        writes = int(reconcile(aadw_notify.api, repository, payload))
    except Exception as exc:
        print(f'AADW fallback completion reconcile failed: {exc}', file=sys.stderr)
        return 1
    print(f'AADW fallback completion reconcile: {writes}件を作成または更新しました。')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
