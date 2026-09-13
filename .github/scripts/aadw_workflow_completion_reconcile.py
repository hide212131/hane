"""Close AADW pending lifecycles when their source workflow ends."""
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
# These workflows have legitimate business-terminal paths that historically
# wrote a PR URL instead of their Actions run URL.  workflow_run completion is
# the first authoritative place where the exact run/attempt is always known.
TERMINAL_ONLY_CONTEXTS = {
    'Claude automatic fix worker': {'hane/claude-fix'},
    'Copilot pre-GUI routing': {'hane/copilot-routing'},
}


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


def marker_targets(call, repository, run_id, attempt):
    """Return PR numbers/SHA evidence from trusted starts for this exact run."""
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
        process, number, sha, marker_run, marker_attempt = match.groups()
        if process not in PROCESS_CONTEXT or marker_run != run_id or marker_attempt != attempt:
            continue
        found.setdefault(int(number), set()).add(sha)
    return found


def trusted_completion_pr(pr, repository):
    owner = repository.split('/', 1)[0]
    return (
        isinstance(pr, dict)
        and pr.get('state') in ('open', 'closed')
        and (pr.get('head', {}).get('repo') or {}).get('full_name') == repository
        and pr.get('user', {}).get('login') in (owner, 'github-actions[bot]', 'claude[bot]')
        and re.fullmatch(r'[0-9a-f]{40}', pr.get('head', {}).get('sha', '')) is not None
    )


def normalize_terminal_only(call, repository, workflow_name, run, number, sha, statuses):
    """Attach run identity to terminal-only statuses and mirror them immediately.

    A terminal status whose URL is only the PR has no trustworthy execution
    identity.  Use the completed workflow_run window to prove ownership, then
    append an equivalent status with the exact Actions run/attempt URL.  The
    append-only correlated status lets scheduled reconciliation recover a lost
    Conversation write without ever inventing a run from a commit-status id.
    """
    contexts = TERMINAL_ONLY_CONTEXTS.get(workflow_name, set())
    if not contexts:
        return 0
    started_at = lifecycle.parse_time(run.get('run_started_at'))
    finished_at = lifecycle.parse_time(run.get('updated_at'))
    if started_at is None or finished_at is None:
        raise RuntimeError('workflow_run terminal normalization requires run_started_at/updated_at')
    run_id = str(run['id'])
    attempt = str(run.get('run_attempt') or '1')
    run_url = f'https://github.com/{repository}/actions/runs/{run_id}/attempts/{attempt}'
    writes = 0
    for row in statuses:
        if row.get('context') not in contexts or row.get('state') == 'pending':
            continue
        explicit_run, _ = lifecycle.status_identity(row)
        created_at = lifecycle.parse_time(row.get('created_at'))
        target_url = row.get('target_url') or ''
        if explicit_run is not None or created_at is None or not (started_at <= created_at <= finished_at):
            continue
        if not re.fullmatch(rf'https://github\.com/{re.escape(repository)}/pull/[1-9][0-9]*', target_url):
            continue
        context = row['context']
        description = row.get('description') or ''
        state = row.get('state') or 'error'
        already = any(
            candidate.get('context') == context
            and candidate.get('state') == state
            and (candidate.get('description') or '') == description
            and candidate.get('target_url') == run_url
            for candidate in statuses
        )
        if not already:
            call(
                f'repos/{repository}/statuses/{sha}',
                payload={
                    'state': state,
                    'context': context,
                    'description': description,
                    'target_url': run_url,
                },
                method='POST',
            )
            writes += 1
        process = lifecycle.CONTEXTS[context]
        notify_state = lifecycle.outcome(context, row)
        result = aadw_notify.notify(
            call,
            state=notify_state,
            process=process,
            kind='pr',
            number=number,
            sha=sha,
            repository=repository,
            run_id=run_id,
            attempt=attempt,
            detail=f'状態: {description}',
        )
        writes += result['action'] != 'noop'
    return writes


def reconcile(call, repository, payload):
    if payload.get('action') != 'completed':
        return 0
    run = payload.get('workflow_run') or {}
    run_id = str(run.get('id') or '')
    attempt = str(run.get('run_attempt') or '1')
    if not re.fullmatch(r'[1-9][0-9]*', run_id) or not re.fullmatch(r'[1-9][0-9]*', attempt):
        raise RuntimeError('invalid workflow_run correlation')
    conclusion = run.get('conclusion') or 'unknown'
    workflow_name = run.get('name') or ''
    writes = 0
    attempt_cache = {}

    starts = marker_targets(call, repository, run_id, attempt)
    prs = {}
    for pr in lifecycle.pages(call, f'repos/{repository}/pulls?state=open'):
        if trusted_completion_pr(pr, repository):
            prs[int(pr['number'])] = pr
    for number in starts:
        if number in prs:
            continue
        pr = call(f'repos/{repository}/pulls/{number}')
        if trusted_completion_pr(pr, repository):
            prs[number] = pr

    for number, pr in prs.items():
        shas = candidate_shas(call, repository, pr, run_id, attempt)
        shas.update(starts.get(number, set()))
        for sha in shas:
            statuses = lifecycle.pages(call, f'repos/{repository}/commits/{sha}/statuses')
            writes += normalize_terminal_only(
                call, repository, workflow_name, run, number, sha, statuses
            )
            for context, process in lifecycle.CONTEXTS.items():
                rows = [row for row in statuses if row.get('context') == context]
                for generation in lifecycle.build_generations(rows):
                    if generation['latest'].get('state') != 'pending' or generation['run_id'] != run_id:
                        continue
                    actual_attempt = lifecycle.resolve_attempt(call, repository, generation, attempt_cache)
                    if actual_attempt != attempt:
                        continue
                    result = aadw_notify.notify(
                        call,
                        state='failure', process=process, kind='pr', number=number,
                        sha=sha, repository=repository, run_id=run_id, attempt=actual_attempt,
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
