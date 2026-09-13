"""Recover recent AADW lifecycles for PRs that closed while processing.

The ordinary periodic reconcilers intentionally enumerate open PRs. This
controller adds a narrow recovery path for closed/merged PRs: only a recent,
trusted github-actions AADW start marker may nominate a PR/SHA for recovery.
It never scans the repository's closed PR list.
"""
import argparse
import datetime as dt
import os
import re
import sys

import aadw_notification_reconcile as lifecycle
import aadw_notify
import aadw_prior_head_reconcile as prior

_MARKER = re.compile(
    r'<!-- hane-aadw: process=([a-z0-9-]+) kind=pr number=([1-9][0-9]*) '
    r'sha=([0-9a-f]{40}) run=([1-9][0-9]*) attempt=([1-9][0-9]*) -->'
)
_RECOVERABLE = set(lifecycle.CONTEXTS.values()) | {'codex-review-fallback'}


def trusted_closed_pr(pr, repository):
    owner = repository.split('/', 1)[0]
    return (
        isinstance(pr, dict)
        and pr.get('state') == 'closed'
        and (pr.get('head', {}).get('repo') or {}).get('full_name') == repository
        and pr.get('user', {}).get('login') in (owner, 'github-actions[bot]', 'claude[bot]')
        and re.fullmatch(r'[0-9a-f]{40}', pr.get('head', {}).get('sha', '')) is not None
    )


def recent_start_targets(call, repository, cutoff):
    """Return exact closed-PR start markers created within the recovery window."""
    found = {}
    for comment in lifecycle.pages(call, f'repos/{repository}/issues/comments?sort=created&direction=desc'):
        if comment.get('user', {}).get('login') != 'github-actions[bot]':
            continue
        body = comment.get('body') or ''
        if '— 処理開始' not in body:
            continue
        match = _MARKER.search(body)
        if not match:
            continue
        process, number, sha, run_id, attempt = match.groups()
        if process not in _RECOVERABLE:
            continue
        created = lifecycle.parse_time(comment.get('created_at'))
        if created is None or created < cutoff:
            continue
        key = (int(number), sha, process, run_id, attempt)
        previous = found.get(key)
        if previous is None or comment.get('id', 0) > previous.get('id', 0):
            found[key] = comment
    return [(*key, comment) for key, comment in found.items()]


def reconcile(call, repository, *, lookback_seconds=7200, now=None):
    now = now or dt.datetime.now(dt.timezone.utc)
    cutoff = now - dt.timedelta(seconds=lookback_seconds)
    targets = recent_start_targets(call, repository, cutoff)
    pr_cache = {}
    status_cache = {}
    writes = 0

    for number, sha, process, run_id, attempt, _comment in targets:
        pr = pr_cache.get(number)
        if pr is None:
            pr = call(f'repos/{repository}/pulls/{number}')
            pr_cache[number] = pr
        if not trusted_closed_pr(pr, repository):
            continue

        statuses = status_cache.get(sha)
        if statuses is None:
            statuses = lifecycle.pages(call, f'repos/{repository}/commits/{sha}/statuses')
            status_cache[sha] = statuses
        writes += prior.reconcile_sha(call, repository, number, sha, statuses, cutoff)

        marker_text = aadw_notify.marker(process, 'pr', number, sha, run_id, attempt)
        existing = aadw_notify.find_comment(call, repository, number, marker_text)
        if existing is None or '— 処理開始' not in (existing.get('body') or ''):
            continue

        run = call(f'repos/{repository}/actions/runs/{run_id}/attempts/{attempt}')
        if not isinstance(run, dict) or run.get('run_attempt') != int(attempt):
            raise RuntimeError(f'closed PR recovery could not verify run {run_id} attempt {attempt}')
        if run.get('status') != 'completed' and not run.get('conclusion'):
            continue
        conclusion = run.get('conclusion') or 'unknown'
        result = aadw_notify.notify(
            call,
            state='failure',
            process=process,
            kind='pr',
            number=number,
            sha=sha,
            repository=repository,
            run_id=run_id,
            attempt=attempt,
            detail=f'workflowが終端状態を記録しないまま終了しました（{conclusion}）。',
        )
        writes += result['action'] != 'noop'
    return writes


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--lookback-seconds', type=int, default=7200)
    args = parser.parse_args()
    repository = os.environ.get('GITHUB_REPOSITORY', '')
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository):
        print('AADW closed-PR reconcile: repositoryが不正です。', file=sys.stderr)
        return 1
    try:
        writes = reconcile(
            aadw_notify.api,
            repository,
            lookback_seconds=max(300, args.lookback_seconds),
        )
    except Exception as exc:
        print(f'AADW closed-PR reconcileに失敗しました: {exc}', file=sys.stderr)
        return 1
    print(f'AADW closed-PR reconcile: {writes}件を作成または更新しました。')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
