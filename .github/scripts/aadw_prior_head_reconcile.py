"""Recover AADW lifecycle comments for recent prior PR heads.

Most AADW status reconciliation can look only at the current PR head. Claude
fix is different: it may publish `pending` on head A, push head B, and only then
publish its terminal status back on A. A current-head-only scan would then leave
A's Conversation comment at "処理開始" forever, or miss the start entirely if
head B arrived before the notification reconciler ran.

This controller therefore combines two trusted sources:
- existing GitHub Actions AADW start comments, which preserve an exact old SHA,
  run id, and attempt even when the head has moved;
- the most recent PR commits, which recover a lifecycle even when its start
  comment was missed before the head changed.

It reuses the status grouping and outcome semantics from
aadw_notification_reconcile and writes through aadw_notify, so a recovered
lifecycle updates the same marker when one already exists and remains idempotent.
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
_RECENT_PRIOR_HEADS = 5


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


def recent_prior_shas(call, repository, pr):
    current_sha = pr['head']['sha']
    rows = lifecycle.pages(call, f'repos/{repository}/pulls/{int(pr["number"])}/commits')
    shas = []
    for row in rows:
        sha = row.get('sha')
        if isinstance(sha, str) and re.fullmatch(r'[0-9a-f]{40}', sha) and sha != current_sha:
            shas.append(sha)
    # AADW automatically stops after three completed fixes. Five prior heads
    # therefore cover the whole normal repair loop with room for manual retry.
    return shas[-_RECENT_PRIOR_HEADS:]


def reconcile_sha(call, repository, number, sha, statuses, cutoff):
    writes = 0
    source = lifecycle.review_source(statuses)
    for context, process in lifecycle.CONTEXTS.items():
        rows = [row for row in statuses if row.get('context') == context]
        for generation in lifecycle.build_generations(rows):
            if not lifecycle.recent(generation, cutoff):
                continue
            action = lifecycle.notify_generation(
                call, repository, number, sha, context, generation,
                source=source if process == 'codex-review' else None,
            )
            writes += action != 'noop'
    return writes


def reconcile_pr(call, repository, pr, cutoff):
    if not lifecycle.trusted_pr(pr, repository):
        return 0
    number = int(pr['number'])
    current_sha = pr['head']['sha']

    # Recent PR commits recover a missed start even when the head moved before
    # the ordinary current-head reconciler observed its pending status.
    candidates = set(recent_prior_shas(call, repository, pr))

    # A trusted start comment may refer to an older SHA outside the bounded
    # commit window. Preserve those explicit correlations as well.
    for _process, sha, _run_id, _attempt, _comment in prior_starts(call, repository, pr, cutoff):
        candidates.add(sha)

    candidates.discard(current_sha)
    writes = 0
    for sha in sorted(candidates):
        statuses = lifecycle.pages(call, f'repos/{repository}/commits/{sha}/statuses')
        writes += reconcile_sha(call, repository, number, sha, statuses, cutoff)
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
