"""Recover AADW lifecycle comments whose process started on a prior PR head.

Most AADW status reconciliation can look only at the current PR head.  Claude
fix is different: it may publish `pending` on head A, push head B, and only then
publish its terminal status back on A.  The ordinary current-head scan would
therefore leave A's Conversation comment at "処理開始" forever.

This recovery controller treats existing trusted AADW start comments as the
correlation record, fetches the durable statuses for their recorded SHA, and
updates the same comment when that exact run/attempt has reached a terminal
state.  It reuses the parsing and status semantics from aadw_notification_reconcile.
"""
import argparse
import datetime as dt
import os
import re
import sys

import aadw_notification_reconcile as lifecycle
import aadw_notify

_MARKER = re.compile(
    r'<!-- hane-aadw: process=([a-z0-9-]+) kind=pr number=([1-9][0-9]*) '
    r'sha=([0-9a-f]{40}) run=([1-9][0-9]*) attempt=([1-9][0-9]*) -->'
)
PROCESS_CONTEXT = {process: context for context, process in lifecycle.CONTEXTS.items()}


def prior_starts(call, repository, pr, cutoff):
    number = int(pr['number'])
    current_sha = pr['head']['sha']
    found = {}
    for comment in lifecycle.pages(call, f'repos/{repository}/issues/{number}/comments'):
        if comment.get('user', {}).get('login') != 'github-actions[bot]':
            continue
        body = comment.get('body') or ''
        if '— 処理開始' not in body:
            continue
        match = _MARKER.search(body)
        if not match:
            continue
        process, marker_number, sha, run_id, attempt = match.groups()
        if marker_number != str(number) or sha == current_sha or process not in PROCESS_CONTEXT:
            continue
        created = lifecycle.parse_time(comment.get('created_at'))
        updated = lifecycle.parse_time(comment.get('updated_at'))
        if not ((created and created >= cutoff) or (updated and updated >= cutoff)):
            continue
        key = (process, sha, run_id, attempt)
        previous = found.get(key)
        if previous is None or comment.get('id', 0) > previous.get('id', 0):
            found[key] = comment
    return [(*key, comment) for key, comment in found.items()]


def matching_generation(statuses, context, run_id, attempt):
    rows = [row for row in statuses if row.get('context') == context]
    candidates = [
        generation for generation in lifecycle.build_generations(rows)
        if generation['run_id'] == run_id and generation['attempt'] == attempt
    ]
    return candidates[-1] if candidates else None


def reconcile_pr(call, repository, pr, cutoff):
    if not lifecycle.trusted_pr(pr, repository):
        return 0
    number = int(pr['number'])
    cache = {}
    writes = 0
    for process, sha, run_id, attempt, _comment in prior_starts(call, repository, pr, cutoff):
        if sha not in cache:
            cache[sha] = lifecycle.pages(call, f'repos/{repository}/commits/{sha}/statuses')
        statuses = cache[sha]
        context = PROCESS_CONTEXT[process]
        generation = matching_generation(statuses, context, run_id, attempt)
        if generation is None or generation['latest'].get('state') == 'pending':
            continue
        source = lifecycle.review_source(statuses) if process == 'codex-review' else None
        action = lifecycle.notify_generation(
            call, repository, number, sha, context, generation, source=source
        )
        writes += action != 'noop'
    return writes


def reconcile(call, repository, *, lookback_seconds=7200, now=None):
    now = now or dt.datetime.now(dt.timezone.utc)
    cutoff = now - dt.timedelta(seconds=lookback_seconds)
    writes = 0
    for pr in lifecycle.pages(call, f'repos/{repository}/pulls?state=open'):
        writes += reconcile_pr(call, repository, pr, cutoff)
    return writes


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--lookback-seconds', type=int, default=7200)
    args = parser.parse_args()
    repository = os.environ.get('GITHUB_REPOSITORY', '')
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository):
        print('AADW prior-head reconcile: repositoryが不正です。', file=sys.stderr)
        return 1
    try:
        writes = reconcile(aadw_notify.api, repository, lookback_seconds=max(300, args.lookback_seconds))
    except Exception as exc:
        print(f'AADW prior-head reconcileに失敗しました: {exc}', file=sys.stderr)
        return 1
    print(f'AADW prior-head reconcile: {writes}件を作成または更新しました。')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
