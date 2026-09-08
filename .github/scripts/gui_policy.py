"""Pure GUI request/receipt policy. No network, tokens, or target execution."""
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re

POLICY = 'v1'
PROCEDURE = 'hosted-gui-interaction/2'
CONTEXT = 'hane/gui-validation'
STATUS = re.compile(r'GUI (pending|pass|fail|blocked) v1 ([0-9a-f]{12}) g([0-9]+-[0-9]+)')
REQUIRED_STEPS = {
    'ascii_edit_save_undo_redo_reopen': {'launch', 'window_discovery', 'edit_save', 'append_save', 'undo_save', 'redo_save', 'capture_before', 'capture_after', 'capture_reopen', 'visible_saved_text', 'reopen_content_check', 'cleanup'},
    'japanese_ime_input': {'query_current_source', 'list_input_sources', 'select_japanese_source', 'launch', 'window_discovery', 'ime_input_save', 'capture_before', 'capture_after', 'cleanup', 'restore_input_source'},
    'os_scroll': {'launch', 'window_discovery', 'capture_before', 'os_wheel', 'capture_after', 'visible_scroll', 'scroll_preserves_document', 'cleanup'},
}
REQUIRED_IMAGES = ['ascii_edit_save_undo_redo_reopen/before.png',
                   'ascii_edit_save_undo_redo_reopen/after.png',
                   'ascii_edit_save_undo_redo_reopen/reopen/reopen.png',
                   'japanese_ime_input/before.png', 'japanese_ime_input/after.png',
                   'os_scroll/before.png', 'os_scroll/after.png']


def latest(statuses):
    result = {}
    for status in statuses:
        context = status.get('context')
        if isinstance(status.get('id'), int) and status['id'] > result.get(context, {}).get('id', -1):
            result[context] = status
    return result


def gui_state(status, sha):
    match = STATUS.fullmatch(status.get('description', ''))
    if not match or match[2] != sha[:12]:
        return None
    outcome = match[1]
    expected = {'pending': 'pending', 'pass': 'success', 'fail': 'failure', 'blocked': 'error'}[outcome]
    return (outcome, match[3]) if status.get('state') == expected else None


def required_from_files(files, expected_count, force=False):
    if not isinstance(files, list) or len(files) != expected_count:
        raise ValueError('incomplete changed-file evidence')
    if force or not files:
        return True
    for item in files:
        if not isinstance(item, dict) or not isinstance(item.get('filename'), str) or not item['filename']:
            raise ValueError('invalid filename')
        names = [item['filename']]
        if 'previous_filename' in item:
            names.append(item['previous_filename'])
        for name in names:
            if not isinstance(name, str) or not name or '..' in name.split('/'):
                raise ValueError('invalid filename')
            if not (name.startswith(('docs/', '.github/')) or ('/' not in name and name.endswith('.md'))):
                return True
    return False


def classification_matches(status, sha, required):
    word = 'required' if required else 'not required'
    return (status.get('description') == f'GUI validation {word} (v1) for {sha[:12]}'
            and status.get('state') == ('failure' if required else 'success'))


def review_ready(statuses, sha):
    review = statuses.get('hane/codex-review', {})
    if review.get('state') == 'success' and review.get('description') == f'Codex review clean for {sha[:12]}':
        return True
    routing = statuses.get('hane/copilot-routing', {})
    return (review.get('state') == 'failure' and review.get('description') == f'Codex findings for {sha[:12]}'
            and routing.get('state') == 'success'
            and routing.get('description') == f'Copilot routing: continue-validation for {sha[:12]}'
            and routing.get('id', 0) > review.get('id', 0))


def parse_time(value):
    return datetime.fromisoformat(value.replace('Z', '+00:00')).astimezone(timezone.utc)


def validate_receipt(raw, request, evidence_dir, job_conclusion, now=None):
    """Fail closed on provenance/shape errors; never upgrade partial evidence."""
    now = now or datetime.now(timezone.utc)
    expected = (('request_id', request['request_id']), ('run_id', request['run_id']),
                ('run_attempt', request['run_attempt']), ('procedure_version', PROCEDURE))
    for name, value in expected:
        if raw.get(name) != value:
            raise ValueError(f'{name} mismatch')
    if raw.get('control', {}).get('sha') != request['control_sha']:
        raise ValueError('control SHA mismatch')
    target = raw.get('target', {})
    if (target.get('actual_sha') != request['sha'] or target.get('expected_sha') != request['sha']
            or target.get('sha_matches') is not True or target.get('working_copy_clean') is not True):
        # A genuine preflight failure is a blocked result, never a pass.
        if raw.get('overall_result') != 'blocked':
            raise ValueError('target identity or clean-checkout mismatch')
    started, finished = parse_time(raw['started_at']), parse_time(raw['finished_at'])
    if not parse_time(request['created_at']) <= started <= finished <= parse_time(request['expires_at']) or finished > now:
        raise ValueError('result outside request lifetime')
    outcome = raw.get('overall_result')
    if outcome not in ('pass', 'fail', 'blocked'):
        raise ValueError('unknown GUI outcome')
    if job_conclusion not in ('success', 'failure'):
        raise ValueError('worker did not finish normally')
    if outcome == 'pass':
        if job_conclusion != 'success':
            raise ValueError('passing payload from unsuccessful worker')
        build = raw.get('build', {})
        if not re.fullmatch('[0-9a-f]{64}', build.get('binary_sha256', '')) or build.get('features') != ['timing-probe']:
            raise ValueError('missing binary identity or wrong build features')
        top = raw.get('top_level_steps', [])
        if {s.get('name') for s in top} != {'preflight', 'build'} or any(s.get('result') != 'pass' for s in top):
            raise ValueError('incomplete build/preflight')
        scenarios = raw.get('scenarios', [])
        if len(scenarios) != len(REQUIRED_STEPS) or {s.get('name') for s in scenarios} != set(REQUIRED_STEPS):
            raise ValueError('required scenarios missing or duplicated')
        for scenario in scenarios:
            steps = scenario.get('steps', [])
            if (scenario.get('result') != 'pass' or any(s.get('result') != 'pass' for s in steps)
                    or not REQUIRED_STEPS[scenario['name']] <= {s.get('name') for s in steps}):
                raise ValueError('required steps missing or not passed')
            if scenario['name'] == 'ascii_edit_save_undo_redo_reopen':
                for name in ('launch', 'window_discovery', 'cleanup'):
                    if sum(s.get('name') == name for s in steps) != 2:
                        raise ValueError('reopen lifecycle incomplete')
        for relative in REQUIRED_IMAGES:
            image = Path(evidence_dir) / relative
            if image.is_symlink() or not image.is_file() or image.stat().st_size < 100:
                raise ValueError('required screenshot missing')
    return outcome


def receipt(request, outcome, reason, raw=None, evidence_dir=None):
    hashes = {}
    if evidence_dir:
        for path in Path(evidence_dir).rglob('*'):
            if path.is_file() and not path.is_symlink():
                hashes[path.relative_to(evidence_dir).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return {'schema_version': 1, 'policy_version': POLICY, 'request': request,
            'outcome': outcome, 'reason': str(reason)[:2000], 'result': raw, 'sha256': hashes}
