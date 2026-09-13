"""Fallback notification from an independent job after worker failure/timeout."""
import json
import os
import re
import subprocess


def api(endpoint, payload=None):
    args = ['gh', 'api', endpoint]
    if payload is not None:
        args += ['--method', 'PATCH' if '/issues/comments/' in endpoint else 'POST', '--input', '-']
    result = subprocess.run(args, input=json.dumps(payload) if payload is not None else None,
                            capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError('通知APIを呼び出せませんでした。')
    return json.loads(result.stdout) if result.stdout.strip() else None


def notify(call, *, repository, number, sha, stage, result, run_id, attempt):
    if (not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository)
            or not re.fullmatch(r'[1-9][0-9]*', number)
            or not re.fullmatch(r'[1-9][0-9]*', run_id)
            or not re.fullmatch(r'[1-9][0-9]*', attempt)
            or stage not in ('implement', 'claude-fix')
            or result not in ('failure', 'cancelled')):
        return False
    base = f'repos/{repository}'
    if stage == 'claude-fix':
        if not re.fullmatch(r'[0-9a-f]{40}', sha):
            return False
        pr = call(f'{base}/pulls/{number}')
        if (pr.get('base', {}).get('repo', {}).get('full_name') != repository
                or pr.get('head', {}).get('repo', {}).get('full_name') != repository
                or pr.get('user', {}).get('login') not in
                (repository.split('/')[0], 'github-actions[bot]', 'claude[bot]')):
            return False
    else:
        sha = 'none'
    marker = f'<!-- hane-stall: number={number} stage={stage} sha={sha} -->'
    run_url = f'https://github.com/{repository}/actions/runs/{run_id}/attempts/{attempt}'
    existing = None
    # Exhaust pagination before posting, so a late existing comment is not missed.
    for page in range(1, 101):
        comments = call(f'{base}/issues/{number}/comments?per_page=100&page={page}')
        if not isinstance(comments, list):
            raise RuntimeError('通知履歴を確認できませんでした。')
        for comment in comments:
            if comment.get('user', {}).get('login') == 'github-actions[bot]' and marker in (comment.get('body') or ''):
                if re.search(re.escape(run_url) + r'(?:[\s)]|$)', comment['body']):
                    # The worker already published a more specific diagnosis.
                    return False
                prior_runs = re.findall(r'/actions/runs/([0-9]+)(?:/attempts/([0-9]+))?', comment['body'])
                if any((int(r), int(a or 1)) > (int(run_id), int(attempt)) for r, a in prior_runs):
                    return False  # An older finishing job cannot overwrite a newer report.
                if existing is None or comment['id'] > existing['id']:
                    existing = comment
        if len(comments) < 100:
            break
    else:
        raise RuntimeError('通知履歴が上限を超えたため重複投稿を避けました。')
    target = f'Issue #{number}' if stage == 'implement' else f'PR #{number} / {sha}'
    body = (f'### Claudeジョブの停止\n\n対象: {target}\n\n'
            'ジョブが正常完了しませんでした。ジョブ全体の時間切れや中止でも、この通知は独立したジョブから送信されます。'
            '途中まで変更が反映された可能性があるため、実行ログと現在のPRを確認してください。'
            '再実行が必要な場合はownerが新しい指示を投稿してください。\n\n'
            f'実行: {run_url}\n\n{marker}\n')
    endpoint = f'{base}/issues/comments/{existing["id"]}' if existing else f'{base}/issues/{number}/comments'
    call(endpoint, {'body': body})
    return True


def main():
    try:
        notify(api, repository=os.environ['REPOSITORY'], number=os.environ['TARGET_NUMBER'],
               sha=os.environ.get('TARGET_SHA', ''), stage=os.environ['STAGE'],
               result=os.environ['WORKER_RESULT'], run_id=os.environ['GITHUB_RUN_ID'],
               attempt=os.environ['GITHUB_RUN_ATTEMPT'])
    except (KeyError, ValueError, TypeError, RuntimeError, subprocess.TimeoutExpired):
        print('停止結果の通知を完了できませんでした。Actionsの実行結果を確認してください。')
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
