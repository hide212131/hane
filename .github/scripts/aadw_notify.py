"""Shared Issue/PR lifecycle comments for AADW user-visible processes."""
import argparse
import json
import os
import re
import subprocess
import sys

PROCESSES = {
    'implement': '/implement によるClaude Code実装',
    'codex-review': 'Codexレビュー',
    'codex-review-fallback': 'Codexレビュー利用上限時のCopilot代替レビュー',
    'copilot-pre-gui-routing': 'Copilot pre-GUI routing',
    'claude-fix': 'Claude Codeによる自動修正',
    'gui-requirement': 'GUI validation要否判定',
    'gui-validation': 'GUI validation',
    'final-judge': 'Copilot final judge / deterministic merge gate',
}
STATE_LABELS = {'start': '処理開始', 'success': '正常終了', 'failure': '異常終了'}

_NUMBER = re.compile(r'[1-9][0-9]*')
_SHA = re.compile(r'[0-9a-f]{40}')
_REPO = re.compile(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+')
_ACTOR = 'github-actions[bot]'


def marker(process, kind, number, sha, run_id, attempt):
    return (f'<!-- hane-aadw: process={process} kind={kind} number={number} '
            f'sha={sha} run={run_id} attempt={attempt} -->')


def api(endpoint, payload=None, method=None):
    args = ['gh', 'api', endpoint]
    if method or payload is not None:
        args += ['--method', method or 'POST', '--input', '-']
    result = subprocess.run(args, input=json.dumps(payload) if payload is not None else None,
                            capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f'AADW通知APIの呼び出しに失敗しました: {endpoint}: {result.stderr[:500]}')
    return json.loads(result.stdout) if result.stdout.strip() else None


def _validate(*, state, process, kind, number, sha, repository, run_id, attempt):
    number, run_id, attempt = str(number), str(run_id), str(attempt)
    sha = sha or 'none'
    if (process not in PROCESSES or state not in STATE_LABELS or kind not in ('issue', 'pr')
            or not _REPO.fullmatch(repository) or not _NUMBER.fullmatch(number)
            or not _NUMBER.fullmatch(run_id) or not _NUMBER.fullmatch(attempt)
            or (sha != 'none' and not _SHA.fullmatch(sha))):
        raise ValueError('invalid AADW notify request')
    return number, sha, run_id, attempt


def find_comments(call, repository, number, marker_text, actor=_ACTOR):
    found = []
    for page in range(1, 101):
        comments = call(f'repos/{repository}/issues/{number}/comments?per_page=100&page={page}')
        if not isinstance(comments, list):
            raise RuntimeError('AADW通知の既存コメントを確認できませんでした。')
        found.extend(
            comment for comment in comments
            if comment.get('user', {}).get('login') == actor
            and marker_text in (comment.get('body') or '')
        )
        if len(comments) < 100:
            return found
    raise RuntimeError('AADW通知の既存コメント件数が上限を超えました。')


def find_comment(call, repository, number, marker_text, actor=_ACTOR):
    found = find_comments(call, repository, number, marker_text, actor)
    return min(found, key=lambda comment: comment.get('id', 0), default=None)


def render(process, state, kind, number, sha, run_id, attempt, repository, detail, marker_text):
    target = f'{"Issue" if kind == "issue" else "PR"} #{number}'
    if sha != 'none':
        target += f' (SHA `{sha[:12]}`)'
    run_url = f'https://github.com/{repository}/actions/runs/{run_id}/attempts/{attempt}'
    lines = [f'### AADW: {PROCESSES[process]} — {STATE_LABELS[state]}', '',
             f'対象: {target}', f'実行: {run_url}']
    if detail:
        lines += ['', detail]
    lines += ['', marker_text]
    return '\n'.join(lines) + '\n'


def _apply_to_existing(call, repository, existing, state, body):
    existing_body = existing.get('body') or ''
    # Lifecycle state is monotonic for one exact run/attempt. A delayed
    # pending-status reconcile must not turn a terminal outcome back into
    # "処理開始" after timeout/cancel or another terminal observation.
    if state == 'start' and ('— 正常終了' in existing_body or '— 異常終了' in existing_body):
        return 'noop'
    if existing_body == body:
        return 'noop'
    call(f'repos/{repository}/issues/comments/{existing["id"]}', {'body': body}, 'PATCH')
    existing['body'] = body
    return 'update'


def notify(call, *, state, process, kind, number, sha, repository, run_id, attempt, detail=''):
    number, sha, run_id, attempt = _validate(
        state=state, process=process, kind=kind, number=number, sha=sha,
        repository=repository, run_id=run_id, attempt=attempt)
    marker_text = marker(process, kind, number, sha, run_id, attempt)
    body = render(process, state, kind, number, sha, run_id, attempt, repository,
                  (detail or '').strip()[:2000], marker_text)
    existing = find_comment(call, repository, number, marker_text)
    if existing:
        action = _apply_to_existing(call, repository, existing, state, body)
        return {'action': action, 'id': existing['id']}

    created = call(f'repos/{repository}/issues/{number}/comments', {'body': body})
    if not isinstance(created, dict) or not isinstance(created.get('id'), int):
        raise RuntimeError('AADW通知コメントの作成結果を確認できませんでした。')

    # Direct producer notifications and scheduled reconcile can both observe
    # "not found" before either POST completes. Re-read after creation and use
    # the oldest bot-owned exact-marker comment as the deterministic canonical
    # record. A later creator deletes only its own duplicate, so concurrent
    # creators cannot delete each other's canonical comment.
    matches = find_comments(call, repository, number, marker_text)
    by_id = {comment.get('id'): comment for comment in matches if isinstance(comment.get('id'), int)}
    by_id.setdefault(created['id'], created)
    canonical = by_id[min(by_id)]
    canonical_action = _apply_to_existing(call, repository, canonical, state, body)

    if created['id'] != canonical['id']:
        call(f'repos/{repository}/issues/comments/{created["id"]}', None, 'DELETE')
        return {'action': 'update' if canonical_action == 'noop' else canonical_action,
                'id': canonical['id']}
    return {'action': 'create' if canonical_action == 'noop' else canonical_action,
            'id': canonical['id']}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('state', choices=tuple(STATE_LABELS))
    parser.add_argument('--process', required=True, choices=tuple(PROCESSES))
    parser.add_argument('--kind', required=True, choices=('issue', 'pr'))
    parser.add_argument('--number', required=True)
    parser.add_argument('--sha', default='none')
    parser.add_argument('--detail', default='')
    args = parser.parse_args()
    try:
        result = notify(api, state=args.state, process=args.process, kind=args.kind,
                        number=args.number, sha=args.sha, detail=args.detail,
                        repository=os.environ['GITHUB_REPOSITORY'],
                        run_id=os.environ['GITHUB_RUN_ID'], attempt=os.environ['GITHUB_RUN_ATTEMPT'])
    except (KeyError, ValueError, TypeError, RuntimeError, subprocess.TimeoutExpired) as exc:
        print(f'AADW状態通知に失敗しました: {exc}', file=sys.stderr)
        return 1
    print(json.dumps(result))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
