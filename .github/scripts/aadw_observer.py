"""Mirror trusted AADW state transitions to Issue/PR lifecycle comments."""
import json
import os
import re
import subprocess
import sys
import time
import aadw_notify

STATUS_PROCESS = {
    'hane/codex-review': 'codex-review',
    'hane/copilot-routing': 'copilot-pre-gui-routing',
    'hane/claude-fix': 'claude-fix',
    'hane/gui-requirement': 'gui-requirement',
    'hane/gui-validation': 'gui-validation',
    'hane/final-judge': 'final-judge',
    'hane/review-source': 'codex-review-fallback',
}
RUN_URL = re.compile(r'/actions/runs/([1-9][0-9]*)(?:/attempts/([1-9][0-9]*))?')
IMPLEMENT_PR = re.compile(r'(?m)^Agentic implementation of #([1-9][0-9]*)\.\s*$')
PROGRESS = re.compile(r'Hane progress run=([1-9][0-9]*) attempt=([1-9][0-9]*)')
LIMIT_TEXTS = (
    'You have reached your Codex usage limits for code reviews.',
    'Codex usage limits have been reached for code reviews.',
)


def api(endpoint, payload=None, method=None):
    args = ['gh', 'api', endpoint]
    if method or payload is not None:
        args += ['--method', method or 'POST', '--input', '-']
    result = subprocess.run(args, input=json.dumps(payload) if payload is not None else None,
                            capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f'GitHub API failed for {endpoint}: {result.stderr[:500]}')
    return json.loads(result.stdout) if result.stdout.strip() else None


def pages(call, endpoint):
    rows = []
    for page in range(1, 101):
        batch = call(endpoint + ('&' if '?' in endpoint else '?') + f'per_page=100&page={page}')
        if not isinstance(batch, list):
            raise RuntimeError('unexpected paginated response')
        rows += batch
        if len(batch) < 100:
            return rows
    raise RuntimeError('pagination bound exceeded')


def trusted_pr(pr, repository):
    owner = repository.split('/', 1)[0]
    return (pr.get('base', {}).get('repo', {}).get('full_name') == repository
            and (pr.get('head', {}).get('repo') or {}).get('full_name') == repository
            and pr.get('user', {}).get('login') in {owner, 'github-actions[bot]', 'claude[bot]'})


def resolve_pr(call, repository, sha, target_url=''):
    match = re.search(r'/pull/([1-9][0-9]*)', target_url or '')
    if match:
        pr = call(f'repos/{repository}/pulls/{match.group(1)}')
        if trusted_pr(pr, repository):
            return pr
        raise RuntimeError('status points to an untrusted PR')
    candidates = [pr for pr in call(f'repos/{repository}/commits/{sha}/pulls') if trusted_pr(pr, repository)]
    exact = [pr for pr in candidates if pr.get('head', {}).get('sha') == sha]
    selected = exact or candidates
    if len({pr.get('number') for pr in selected}) != 1:
        raise RuntimeError('cannot resolve one trusted PR for AADW status')
    return selected[0]


def run_from_url(call, repository, url, fallback_run, fallback_attempt):
    match = RUN_URL.search(url or '')
    if not match:
        return str(fallback_run), str(fallback_attempt)
    run_id, attempt = match.group(1), match.group(2)
    if attempt is None:
        run = call(f'repos/{repository}/actions/runs/{run_id}')
        value = run.get('run_attempt') if isinstance(run, dict) else None
        attempt = str(value) if isinstance(value, int) and value > 0 else '1'
    return run_id, attempt


def source_run(call, repository, event, observer_run, observer_attempt):
    if RUN_URL.search(event.get('target_url') or ''):
        return run_from_url(call, repository, event['target_url'], observer_run, observer_attempt)
    if event.get('state') != 'pending':
        rows = pages(call, f'repos/{repository}/commits/{event["sha"]}/statuses')
        prior = sorted((r for r in rows if r.get('context') == event.get('context')
                        and isinstance(r.get('id'), int) and r['id'] < event['id']),
                       key=lambda r: r['id'], reverse=True)
        for row in prior:
            if row.get('state') == 'pending':
                return run_from_url(call, repository, row.get('target_url') or '', observer_run, observer_attempt)
            if row.get('state') in ('success', 'failure', 'error'):
                break
    return str(observer_run), str(observer_attempt)


def active_run(call, repository, number, process, sha):
    pattern = re.compile(rf'process={re.escape(process)} kind=pr number={number} sha={sha} '
                         r'run=([1-9][0-9]*) attempt=([1-9][0-9]*) -->')
    found = []
    for comment in pages(call, f'repos/{repository}/issues/{number}/comments'):
        body = comment.get('body') or ''
        match = pattern.search(body)
        if comment.get('user', {}).get('login') == 'github-actions[bot]' and match and '— 処理開始' in body:
            found.append((comment.get('id', 0), match.group(1), match.group(2)))
    return max(found)[1:] if found else None


def status_outcome(context, state, description):
    if state == 'pending':
        return 'start', '処理を開始しました。'
    if context == 'hane/codex-review':
        if description.startswith('Codex review clean'):
            return 'success', 'レビュー結果: clean'
        if description.startswith('Codex findings'):
            return 'success', 'レビュー結果: findings'
        return 'failure', 'Codexレビューを正常に完了できませんでした。'
    if context == 'hane/review-source':
        return (('success', 'Copilotによる代替レビューが完了しました。')
                if state == 'success' and description.startswith('Review source: Copilot fallback')
                else ('failure', 'Copilotによる代替レビューを正常に完了できませんでした。'))
    if context == 'hane/copilot-routing':
        if 'controller failed' in description.lower():
            return 'failure', 'Copilot routing controllerを正常に完了できませんでした。'
        match = re.search(r'Copilot routing: ([a-z-]+)', description)
        return 'success', f'判定結果: {match.group(1) if match else "完了"}'
    if context == 'hane/claude-fix':
        return (('success', 'Claudeによる修正が完了しました。')
                if state == 'success' and description.startswith('Claude fix completed')
                else ('failure', 'Claudeによる修正を正常に完了できませんでした。'))
    if context == 'hane/gui-requirement':
        if state == 'success':
            return 'success', '判定結果: GUI validation不要'
        if state == 'failure':
            return 'success', '判定結果: GUI validation必要'
        return 'failure', 'GUI validation要否判定を正常に完了できませんでした。'
    if context == 'hane/gui-validation':
        match = re.search(r'\bGUI (pending|pass|fail|blocked|superseded)\b', description)
        value = match.group(1) if match else ''
        return (('success', f'検証結果: {value}') if value in ('pass', 'fail')
                else ('failure', f'GUI validationを正常に完了できませんでした{f"（{value}）" if value else ""}。'))
    if context == 'hane/final-judge':
        match = re.search(r'\bFinal (pending|ready|merged|fix|blocked)\b', description)
        value = match.group(1) if match else ''
        return (('success', f'判定結果: {value}') if value in ('ready', 'merged', 'fix', 'blocked')
                else ('failure', 'final judgeを正常に完了できませんでした。'))
    raise ValueError('unsupported AADW status')


def latest_progress(call, repository, issue_number):
    found = []
    for comment in pages(call, f'repos/{repository}/issues/{issue_number}/comments'):
        if comment.get('user', {}).get('login') == 'claude[bot]':
            match = PROGRESS.search(comment.get('body') or '')
            if match:
                found.append((comment.get('id', 0), match.group(1), match.group(2)))
    if not found:
        raise RuntimeError('implementation progress correlation was not found')
    return max(found)[1:]


def finish_implement(call, repository, pr):
    match = IMPLEMENT_PR.search(pr.get('body') or '')
    if not match or not trusted_pr(pr, repository):
        return False
    issue = match.group(1)
    run_id, attempt = latest_progress(call, repository, issue)
    aadw_notify.notify(call, state='success', process='implement', kind='issue', number=issue, sha='none',
                       repository=repository, run_id=run_id, attempt=attempt,
                       detail=f'実装PR #{pr["number"]} を作成しました。')
    return True


def handle_status(call, repository, event, observer_run, observer_attempt):
    if event.get('sender', {}).get('login') != 'github-actions[bot]':
        return False
    context, sha = event.get('context') or '', event.get('sha') or ''
    if not re.fullmatch(r'[0-9a-f]{40}', sha):
        raise RuntimeError('invalid AADW status sha')
    pr = resolve_pr(call, repository, sha, event.get('target_url') or '')
    if context == 'hane/trusted-ci-generation':
        return finish_implement(call, repository, pr)
    if context not in STATUS_PROCESS:
        return False
    if context == 'hane/codex-review' and event.get('state') != 'pending':
        rows = pages(call, f'repos/{repository}/commits/{sha}/statuses')
        if any(r.get('context') == 'hane/review-source' and r.get('state') == 'success'
               and (r.get('description') or '').startswith('Review source: Copilot fallback')
               and r.get('id', -1) < event.get('id', 0) for r in rows):
            return False
    state, detail = status_outcome(context, event.get('state') or '', event.get('description') or '')
    correlation = active_run(call, repository, pr['number'], 'codex-review-fallback', sha) \
        if context == 'hane/review-source' and event.get('state') != 'pending' else None
    run_id, attempt = correlation or source_run(call, repository, event, observer_run, observer_attempt)
    aadw_notify.notify(call, state=state, process=STATUS_PROCESS[context], kind='pr', number=pr['number'],
                       sha=sha, repository=repository, run_id=run_id, attempt=attempt, detail=detail)
    return True


def implement_issue_for_run(call, repository, run_id, attempt):
    marker = f'Hane progress run={run_id} attempt={attempt}'
    found = []
    for comment in pages(call, f'repos/{repository}/issues/comments?sort=created&direction=desc'):
        match = re.search(r'/issues/([1-9][0-9]*)$', comment.get('issue_url') or '')
        if comment.get('user', {}).get('login') == 'claude[bot]' and marker in (comment.get('body') or '') and match:
            found.append((comment.get('id', 0), match.group(1)))
    if not found:
        raise RuntimeError('implementation Issue correlation was not found')
    return max(found)[1]


def implementation_pr(call, repository, issue):
    marker = f'Agentic implementation of #{issue}.'
    rows = [pr for pr in pages(call, f'repos/{repository}/pulls?state=all')
            if marker in (pr.get('body') or '') and trusted_pr(pr, repository)]
    return max(rows, key=lambda pr: pr.get('number', 0), default=None)


def pending_for_run(call, repository, run_id):
    needle, found = f'/actions/runs/{run_id}', []
    for pr in pages(call, f'repos/{repository}/pulls?state=open'):
        if not trusted_pr(pr, repository):
            continue
        sha = pr.get('head', {}).get('sha') or ''
        latest = {}
        for row in pages(call, f'repos/{repository}/commits/{sha}/statuses'):
            context = row.get('context')
            if context in STATUS_PROCESS and isinstance(row.get('id'), int) and row['id'] > latest.get(context, {}).get('id', -1):
                latest[context] = row
        found += [(pr, row) for row in latest.values()
                  if row.get('state') == 'pending' and needle in (row.get('target_url') or '')]
    return found


def fallback_target(call, repository, run_id, attempt):
    fragment = f'process=codex-review-fallback kind=pr '
    run = f'run={run_id} attempt={attempt} -->'
    found = []
    for pr in pages(call, f'repos/{repository}/pulls?state=open'):
        if not trusted_pr(pr, repository):
            continue
        for comment in pages(call, f'repos/{repository}/issues/{pr["number"]}/comments'):
            body = comment.get('body') or ''
            sha = re.search(r'sha=([0-9a-f]{40})', body)
            if comment.get('user', {}).get('login') == 'github-actions[bot]' and fragment in body and run in body and '— 処理開始' in body and sha:
                found.append((comment.get('id', 0), pr, sha.group(1)))
    return max(found, key=lambda row: row[0], default=None)


def handle_workflow_run(call, repository, payload):
    if payload.get('action') != 'completed':
        return False
    run = payload.get('workflow_run') or {}
    run_id, attempt = str(run.get('id') or ''), str(run.get('run_attempt') or '1')
    if not re.fullmatch(r'[1-9][0-9]*', run_id) or not re.fullmatch(r'[1-9][0-9]*', attempt):
        raise RuntimeError('invalid workflow_run correlation')
    if run.get('name') == 'Implement issue with Claude' and run.get('event') == 'issue_comment':
        issue = implement_issue_for_run(call, repository, run_id, attempt)
        pr = implementation_pr(call, repository, issue) if run.get('conclusion') == 'success' else None
        if pr:
            finish_implement(call, repository, pr)
        else:
            detail = ('実装workflowは終了しましたが、対応する実装PRを確認できませんでした。'
                      if run.get('conclusion') == 'success'
                      else f'実装workflowが正常終了しませんでした（{run.get("conclusion") or "unknown"}）。')
            aadw_notify.notify(call, state='failure', process='implement', kind='issue', number=issue, sha='none',
                               repository=repository, run_id=run_id, attempt=attempt, detail=detail)
        return True
    if run.get('name') == 'Codex limit Copilot review fallback' and run.get('conclusion') != 'success':
        target = fallback_target(call, repository, run_id, attempt)
        if target:
            _, pr, sha = target
            aadw_notify.notify(call, state='failure', process='codex-review-fallback', kind='pr', number=pr['number'],
                               sha=sha, repository=repository, run_id=run_id, attempt=attempt,
                               detail=f'Copilot代替レビューworkflowが正常終了しませんでした（{run.get("conclusion") or "unknown"}）。')
            return True
    handled = False
    for pr, row in pending_for_run(call, repository, run_id):
        aadw_notify.notify(call, state='failure', process=STATUS_PROCESS[row['context']], kind='pr', number=pr['number'],
                           sha=pr['head']['sha'], repository=repository, run_id=run_id, attempt=attempt,
                           detail=f'workflowが終端状態を記録しないまま終了しました（{run.get("conclusion") or "unknown"}）。')
        handled = True
    return handled


def handle_repository_dispatch(call, repository, payload):
    if payload.get('action') != 'claude-progress-start':
        return False
    data = payload.get('client_payload') or {}
    if data.get('repository') != repository:
        return False
    aadw_notify.notify(call, state='start', process='implement', kind='issue', number=data.get('issue_number'), sha='none',
                       repository=repository, run_id=data.get('implementation_run_id'),
                       attempt=data.get('implementation_run_attempt'), detail='Claude Codeによる実装を開始しました。')
    return True


def fallback_run(call, repository, title, created_at):
    for retry in range(10):
        data = call(f'repos/{repository}/actions/runs?event=issue_comment&per_page=100')
        rows = data.get('workflow_runs', []) if isinstance(data, dict) else []
        matches = [r for r in rows if r.get('name') == 'Codex limit Copilot review fallback'
                   and r.get('display_title') == title and r.get('actor', {}).get('login') == 'chatgpt-codex-connector[bot]'
                   and (not created_at or (r.get('created_at') or '') >= created_at)]
        if matches:
            selected = min(matches, key=lambda r: r.get('created_at') or '')
            if isinstance(selected.get('id'), int) and isinstance(selected.get('run_attempt') or 1, int):
                return str(selected['id']), str(selected.get('run_attempt') or 1)
        if retry < 9:
            time.sleep(2)
    raise RuntimeError('Codex fallback workflow run correlation was not found')


def handle_issue_comment(call, repository, payload, observer_run, observer_attempt):
    issue, comment = payload.get('issue') or {}, payload.get('comment') or {}
    body = comment.get('body') or ''
    if (payload.get('action') != 'created' or not issue.get('pull_request')
            or comment.get('user', {}).get('login') != 'chatgpt-codex-connector[bot]'
            or not any(text in body for text in LIMIT_TEXTS)):
        return False
    pr = call(f'repos/{repository}/pulls/{issue["number"]}')
    if not trusted_pr(pr, repository) or pr.get('state') != 'open' or pr.get('draft') is not False:
        return False
    sha = pr.get('head', {}).get('sha') or ''
    rows = pages(call, f'repos/{repository}/commits/{sha}/statuses')
    pending = max((r for r in rows if r.get('context') == 'hane/codex-review' and r.get('state') == 'pending'),
                  key=lambda r: r.get('id', -1), default=None)
    if pending:
        run_id, attempt = run_from_url(call, repository, pending.get('target_url') or '', observer_run, observer_attempt)
        aadw_notify.notify(call, state='failure', process='codex-review', kind='pr', number=pr['number'], sha=sha,
                           repository=repository, run_id=run_id, attempt=attempt,
                           detail='Codexのコードレビュー利用上限に達したため、通常レビューを完了できませんでした。')
    run_id, attempt = fallback_run(call, repository, issue.get('title') or '', comment.get('created_at') or '')
    aadw_notify.notify(call, state='start', process='codex-review-fallback', kind='pr', number=pr['number'], sha=sha,
                       repository=repository, run_id=run_id, attempt=attempt,
                       detail='Codexの利用上限を検出し、Copilot代替レビューを開始しました。')
    return True


def main():
    try:
        repository = os.environ['GITHUB_REPOSITORY']
        event = os.environ['GITHUB_EVENT_NAME']
        with open(os.environ['GITHUB_EVENT_PATH'], encoding='utf-8') as stream:
            payload = json.load(stream)
        if event == 'status':
            handled = handle_status(api, repository, payload, os.environ['GITHUB_RUN_ID'], os.environ['GITHUB_RUN_ATTEMPT'])
        elif event == 'repository_dispatch':
            handled = handle_repository_dispatch(api, repository, payload)
        elif event == 'workflow_run':
            handled = handle_workflow_run(api, repository, payload)
        elif event == 'issue_comment':
            handled = handle_issue_comment(api, repository, payload, os.environ['GITHUB_RUN_ID'], os.environ['GITHUB_RUN_ATTEMPT'])
        else:
            handled = False
        print('AADW lifecycle event handled.' if handled else 'AADW lifecycle event ignored.')
        return 0
    except (KeyError, ValueError, TypeError, RuntimeError, OSError, json.JSONDecodeError, subprocess.TimeoutExpired) as exc:
        print(f'AADW lifecycle notification failed: {exc}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
