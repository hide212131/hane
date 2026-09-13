"""Close an active Copilot fallback lifecycle when its workflow completes."""
import json
import os
import sys

import aadw_notify
import aadw_observer


def reconcile(call, repository, payload):
    if payload.get('action') != 'completed':
        return 0
    run = payload.get('workflow_run') or {}
    if run.get('name') != 'Codex limit Copilot review fallback':
        return 0
    run_id = str(run.get('id') or '')
    attempt = str(run.get('run_attempt') or '1')
    target = aadw_observer.fallback_target(call, repository, run_id, attempt)
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
