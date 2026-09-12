"""Gate AADW lifecycle observation to workflow runs that actually executed work."""
import json
import os
import re
import subprocess
import sys


def api(endpoint):
    result = subprocess.run(
        ['gh', 'api', endpoint], capture_output=True, text=True, timeout=30
    )
    if result.returncode:
        raise RuntimeError(f'GitHub API failed for {endpoint}: {result.stderr[:500]}')
    return json.loads(result.stdout) if result.stdout.strip() else None


def should_observe(call, repository, payload):
    run = payload.get('workflow_run')
    if not isinstance(run, dict):
        return True
    if run.get('name') != 'Implement issue with Claude' or run.get('event') != 'issue_comment':
        return True

    run_id = str(run.get('id') or '')
    attempt = str(run.get('run_attempt') or '1')
    if not re.fullmatch(r'[1-9][0-9]*', run_id) or not re.fullmatch(r'[1-9][0-9]*', attempt):
        raise RuntimeError('invalid implement workflow_run correlation')

    response = call(
        f'repos/{repository}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100'
    )
    jobs = response.get('jobs') if isinstance(response, dict) else None
    if not isinstance(jobs, list):
        raise RuntimeError('implement workflow jobs could not be inspected')
    matches = [job for job in jobs if job.get('name') == 'implement']
    if len(matches) != 1:
        raise RuntimeError('implement workflow did not expose exactly one implement job')
    return matches[0].get('conclusion') != 'skipped'


def main():
    repository = os.environ.get('GITHUB_REPOSITORY', '')
    event_path = os.environ.get('GITHUB_EVENT_PATH', '')
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository):
        print('AADW observer gate: repository is invalid.', file=sys.stderr)
        return 1
    try:
        with open(event_path, encoding='utf-8') as handle:
            payload = json.load(handle)
        print('true' if should_observe(api, repository, payload) else 'false')
    except (OSError, ValueError, TypeError, RuntimeError, subprocess.TimeoutExpired) as exc:
        print(f'AADW observer gate failed: {exc}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
