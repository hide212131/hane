"""Shared Issue/PR start/success/failure notifier for AADW user-visible processes.

Several AADW processes (Codex review, GUI requirement classification, GUI
validation, Copilot routing/judge, ...) only ever recorded their state as a
commit status, which is not visible from the Issue/Pull Request Conversation
tab without opening Actions. This module posts a small, human-readable
Conversation comment mirroring that state instead.

Identity for a single notified execution is
(process, kind, number, sha, run_id, run_attempt): a retry/rerun (new
run_id/attempt) or a new head sha always gets its own comment, so it can never
overwrite a different execution's result, while re-notifying the same
execution (start, then later success/failure; or reprocessing the same event)
updates that one comment in place instead of duplicating it.

Process names are a fixed, trusted table (`PROCESSES`); callers must not pass
free-form text taken from Issue/PR bodies or comments as the process name.
"""
import argparse
import json
import os
import re
import subprocess
import sys

# Trusted, fixed labels for the AADW processes this module can notify about.
# Keep in sync with docs/agentic-development-workflow.md.
PROCESSES = {
    'implement': '/implement によるClaude Code実装',
    'codex-review': 'Codexレビュー',
    'codex-review-fallback': 'Codexレビュー使用量上限時のCopilot fallback',
    'copilot-pre-gui-routing': 'Copilot pre-GUI routing',
    'claude-fix': 'Claude Codeによる自動修正',
    'gui-requirement': 'GUI validation要否判定',
    'gui-validation': 'GUI validation',
    'final-judge': 'Copilot final judge / deterministic merge gate',
    'reconcile': 'AADW reconcile/retry',
}

STATE_LABELS = {'start': '開始', 'success': '正常終了', 'failure': '異常終了'}

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


def find_comment(call, repository, number, marker_text, actor=_ACTOR):
    # Exhaust pagination so a comment on a later page is never missed and
    # duplicated by mistake.
    for page in range(1, 101):
        comments = call(f'repos/{repository}/issues/{number}/comments?per_page=100&page={page}')
        if not isinstance(comments, list):
            raise RuntimeError('AADW通知の既存コメントを確認できませんでした。')
        for comment in comments:
            if comment.get('user', {}).get('login') == actor and marker_text in (comment.get('body') or ''):
                return comment
        if len(comments) < 100:
            return None
    raise RuntimeError('AADW通知の既存コメント件数が上限を超えました。')


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


def notify(call, *, state, process, kind, number, sha, repository, run_id, attempt, detail=''):
    number, run_id, attempt = str(number), str(run_id), str(attempt)
    sha = sha or 'none'
    if (process not in PROCESSES or state not in STATE_LABELS or kind not in ('issue', 'pr')
            or not _REPO.fullmatch(repository) or not _NUMBER.fullmatch(number)
            or not _NUMBER.fullmatch(run_id) or not _NUMBER.fullmatch(attempt)
            or (sha != 'none' and not _SHA.fullmatch(sha))):
        raise ValueError('invalid AADW notify request')
    marker_text = marker(process, kind, number, sha, run_id, attempt)
    body = render(process, state, kind, number, sha, run_id, attempt, repository,
                  (detail or '').strip()[:2000], marker_text)
    existing = find_comment(call, repository, number, marker_text)
    if existing:
        if (existing.get('body') or '') == body:
            return {'action': 'noop', 'id': existing['id']}
        call(f'repos/{repository}/issues/comments/{existing["id"]}', {'body': body}, 'PATCH')
        return {'action': 'update', 'id': existing['id']}
    # No prior comment: either this is a fresh "start", or a terminal call
    # arrives with no matching "start" (e.g. the worker crashed before
    # posting one). Post the outcome either way rather than staying silent.
    created = call(f'repos/{repository}/issues/{number}/comments', {'body': body})
    return {'action': 'create', 'id': created['id']}


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
