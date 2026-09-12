"""Close AADW pending lifecycles on prior PR heads when their source workflow ends."""
import json
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


def candidate_shas(call, repository, pr, run_id, attempt):
    number = int(pr['number'])
    current = pr['head']['sha']
    shas = {current}
    for commit in lifecycle.pages(call, f'repos/{repository}/pulls/{number}/commits'):
        sha = commit.get('sha')
        if isinstance(sha, str) and re.fullmatch(r'[0-9a-f]{40}', sha):
            shas.add(sha)
    for comment in lifecycle.pages(call, f'repos/{repository}/issues/{number}/comments'):
        if comment.get('user', {}).get('login') != 'github-actions[bot]':
            continue
        body = comment.get('body') or ''
        if '— 処理開始' not in body:
            continue
        match = _MARKER.search(body)
        if not match:
            continue
        process, marker_number, sha, marker_run, marker_attempt = match.groups()
        if (marker_number == str(number) and process in PROCESS_CONTEXT
                and marker_run == run_id and marker_attempt == attempt):
            shas.add(sha)
    return shas


def reconcile(call, repository, payload):
    if payload.get('action') != 'completed':
        return 0
    run = payload.get('workflow_run') or {}
    run_id = str(run.get('id') or '')
    attempt = str(run.get('run_attempt') or '1')
    if not re.fullmatch(r'[1-9][0-9]*', run_id) or not re.fullmatch(r'[1-9][0-9]*', attempt):
        raise RuntimeError('invalid workflow_run correlation')
    conclusion = run.get('conclusion') or 'unknown'
    writes = 0
    for pr in lifecycle.pages(call, f'repos/{repository}/pulls?state=open'):
        if not lifecycle.trusted_pr(pr, repository):
            continue
        number = int(pr['number'])
        for sha in candidate_shas(call, repository, pr, run_id, attempt):
            statuses = lifecycle.pages(call, f'repos/{repository}/commits/{sha}/statuses')
            for context, process in lifecycle.CONTEXTS.items():
                rows = [row for row in statuses if row.get('context') == context]
                for generation in lifecycle.build_generations(rows):
                    if (generation['latest'].get('state') != 'pending'
                            or generation['run_id'] != run_id
                            or generation['attempt'] != attempt):
                        continue
                    result = aadw_notify.notify(
                        call,
                        state='failure', process=process, kind='pr', number=number,
                        sha=sha, repository=repository, run_id=run_id, attempt=attempt,
                        detail=f'workflowが終端状態を記録しないまま終了しました（{conclusion}）。',
                    )
                    writes += result['action'] != 'noop'
    return writes


def main():
    repository = os.environ.get('GITHUB_REPOSITORY', '')
    try:
        with open(os.environ['GITHUB_EVENT_PATH'], encoding='utf-8') as stream:
            payload = json.load(stream)
        writes = reconcile(aadw_notify.api, repository, payload)
    except Exception as exc:
        print(f'AADW workflow completion reconcile failed: {exc}', file=sys.stderr)
        return 1
    print(f'AADW workflow completion reconcile: {writes}件を作成または更新しました。')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
