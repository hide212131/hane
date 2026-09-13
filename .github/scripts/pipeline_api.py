"""Small trusted-controller GitHub client shared by GUI and final routing."""
import json
import os
import re
import subprocess
from gui_policy import latest, required_from_files, classification_matches, review_ready

CI_NAMES = ('cargo test / clippy (macos-latest)', 'cargo test / clippy (windows-latest)')


class GitHub:
    def __init__(self, repository=None):
        self.repository = repository or os.environ['GITHUB_REPOSITORY']
        if not re.fullmatch(r'[\w.-]+/[\w.-]+', self.repository):
            raise ValueError('invalid repository')

    def api(self, path, body=None, method=None):
        args = ['gh', 'api', path]
        if method or body is not None:
            args += ['--method', method or 'POST']
        if body is not None:
            args += ['--input', '-']
        completed = subprocess.run(args, input=json.dumps(body) if body is not None else None,
                                   text=True, capture_output=True, timeout=60)
        if completed.returncode:
            raise RuntimeError(f'GitHub API failed for {path}: {completed.stderr[:500]}')
        return json.loads(completed.stdout) if completed.stdout.strip() else None

    def pages(self, path, key=None):
        records = []
        for page in range(1, 101):
            data = self.api(path + ('&' if '?' in path else '?') + f'per_page=100&page={page}')
            rows = data[key] if key else data
            if not isinstance(rows, list):
                raise ValueError('unexpected API list')
            records.extend(rows)
            if len(rows) < 100:
                return records
        raise ValueError('API pagination bound exceeded')

    def repo(self, suffix):
        return f'repos/{self.repository}/{suffix}'

    def pr(self, number):
        return self.api(self.repo(f'pulls/{int(number)}'))

    def trusted(self, pr):
        return (pr.get('state') == 'open' and pr.get('draft') is False
                and (pr.get('head', {}).get('repo') or {}).get('full_name') == self.repository
                and pr.get('base', {}).get('ref') == 'main'
                and pr.get('user', {}).get('login') in (self.repository.split('/')[0], 'github-actions[bot]', 'claude[bot]')
                and re.fullmatch('[0-9a-f]{40}', pr.get('head', {}).get('sha', '')) is not None)

    def statuses(self, sha):
        return latest(self.pages(self.repo(f'commits/{sha}/statuses')))

    def post_status(self, sha, context, state, description, run_id=None):
        run_id = run_id or os.environ['GITHUB_RUN_ID']
        return self.api(self.repo(f'statuses/{sha}'), {'context': context, 'state': state,
                         'description': description, 'target_url': f'https://github.com/{self.repository}/actions/runs/{run_id}'})

    def ci_ready(self, sha, statuses):
        return self.ci_evidence(sha, statuses)['ready']

    def ci_evidence(self, sha, statuses):
        generation = statuses.get('hane/trusted-ci-generation')
        if generation:
            ready = (generation.get('state') == 'success'
                     and re.fullmatch(r'Trusted CI generation .+ passed', generation.get('description', '')) is not None
                     and all(statuses.get(name, {}).get('state') == 'success'
                             and statuses[name].get('target_url') == generation.get('target_url') for name in CI_NAMES))
            return {'head_sha': sha, 'source': 'trusted-ci-generation', 'ready': ready,
                    'generation': {k: generation.get(k) for k in ('id', 'state', 'description', 'target_url')},
                    'checks': [{'name': name, **{k: statuses.get(name, {}).get(k)
                                for k in ('id', 'state', 'target_url')}} for name in CI_NAMES]}
        checks = self.pages(self.repo(f'commits/{sha}/check-runs'), 'check_runs')
        selected = []
        for name in CI_NAMES:
            candidates = [c for c in checks if c.get('name') == name and c.get('head_sha') == sha
                          and c.get('app', {}).get('slug') == 'github-actions']
            check = max(candidates, key=lambda c: c['id'], default={})
            selected.append({'name': name, **{k: check.get(k)
                             for k in ('id', 'head_sha', 'status', 'conclusion', 'html_url')}})
        # Include the regression-test job and any other failed CI step.
        runs = self.api(self.repo(f'actions/runs?head_sha={sha}&event=pull_request&per_page=100'))['workflow_runs']
        ci = max((r for r in runs if r.get('path') == '.github/workflows/ci.yml'), key=lambda r: r['id'], default={})
        ready = (all(c['status'] == 'completed' and c['conclusion'] == 'success' for c in selected)
                 and ci.get('status') == 'completed' and ci.get('conclusion') == 'success')
        return {'head_sha': sha, 'source': 'github-actions-check-runs', 'ready': ready, 'checks': selected,
                'workflow': {k: ci.get(k) for k in ('id', 'head_sha', 'path', 'status', 'conclusion', 'html_url')}}

    def evidence(self, pr, statuses=None):
        sha = pr['head']['sha']
        statuses = self.statuses(sha) if statuses is None else statuses
        files = self.pages(self.repo(f'pulls/{pr["number"]}/files'))
        force = any(label['name'] == 'gui-validation-required' for label in pr.get('labels', []))
        required = required_from_files(files, pr['changed_files'], force)
        classified = classification_matches(statuses.get('hane/gui-requirement', {}), sha, required)
        ci = self.ci_evidence(sha, statuses)
        return {'pr': pr, 'sha': sha, 'statuses': statuses, 'files': files, 'gui_required': required,
                'classified': classified, 'review_ready': review_ready(statuses, sha),
                'ci_ready': ci['ready'], 'ci_evidence': ci}
