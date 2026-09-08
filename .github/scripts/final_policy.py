"""Deterministic final gate. Copilot may recommend, but cannot waive evidence."""
import hashlib
import json
import re
from gui_policy import CONTEXT as GUI_CONTEXT, PROCEDURE, gui_state
from pipeline_api import CI_NAMES

CONTEXT = 'hane/final-judge'
AUTO_LABEL = 'agentic-auto-merge'
STATUS = re.compile(r'Final (pending|ready|fix|blocked|merged) v1 ([0-9a-f]{12}) e([0-9a-f]{16})')


def fingerprint(snapshot):
    return hashlib.sha256(json.dumps(snapshot, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()[:16]


def parse_decision(text):
    value = json.loads(text)
    if (not isinstance(value, dict) or set(value) != {'decision', 'reason'}
            or value['decision'] not in ('ready', 'fix', 'blocked')
            or not isinstance(value['reason'], str) or not 1 <= len(value['reason']) <= 2000):
        raise ValueError('invalid Copilot final decision')
    return value


def final_state(status, sha, key):
    match = STATUS.fullmatch(status.get('description', ''))
    if not match or match[2] != sha[:12] or match[3] != key:
        return None
    expected = {'pending': 'pending', 'ready': 'success', 'merged': 'success', 'fix': 'failure', 'blocked': 'error'}
    return match[1] if status.get('state') == expected[match[1]] else None


def gate(snapshot):
    """Every condition must be known and current. Returns actionable denials."""
    errors = []
    for name in ('trusted', 'ci_ready', 'review_ready', 'classified', 'mergeable'):
        if snapshot.get(name) is not True:
            errors.append(name)
    if snapshot.get('unresolved_threads'):
        errors.append('unresolved review threads')
    if snapshot.get('blocking_reviews'):
        errors.append('changes-requested review')
    if snapshot.get('required_checks') != sorted(CI_NAMES):
        errors.append('unexpected or missing repository merge rules')
    if snapshot.get('gui_required'):
        proof = snapshot.get('gui_receipt') or {}
        if proof.get('outcome') != 'pass':
            errors.append('GUI is not pass')
    if snapshot.get('workflow_changes'):
        errors.append('workflow changes require owner merge')
    return errors


def may_judge(snapshot):
    # Every genuine terminal GUI result reaches the judge, including failures
    # and recovered blocked results even if another gate has since regressed.
    if (snapshot.get('gui_receipt') or {}).get('outcome') in ('pass', 'fail', 'blocked'):
        return True
    return (snapshot.get('trusted') is True and snapshot.get('classified') is True
            and snapshot.get('gui_required') is False and snapshot.get('ci_ready') is True
            and snapshot.get('review_ready') is True)


def authenticated_receipt(proof, snapshot_sha, pr_number, repository, state, run):
    outcome, generation = state
    run_id, attempt = generation.split('-')
    request = proof.get('request', {})
    if (run.get('status') != 'completed' or str(run.get('id')) != run_id
            or str(run.get('run_attempt')) != attempt
            or run.get('path') != '.github/workflows/gui-validation.yml'
            or run.get('head_branch') != 'main'
            or run.get('event') not in ('workflow_dispatch', 'workflow_run', 'schedule')
            or request.get('control_sha') != run.get('head_sha')
            or request.get('procedure_version') != PROCEDURE
            or proof.get('schema_version') != 1 or proof.get('policy_version') != 'v1'
            or request.get('sha') != snapshot_sha or request.get('pr_number') != pr_number
            or request.get('repository') != repository or request.get('generation') != generation
            or request.get('run_id') != run_id or request.get('run_attempt') != attempt
            or request.get('request_id') != f'gui-{generation}-pr{pr_number}'
            or proof.get('outcome') != outcome or outcome == 'pending'):
        raise ValueError('GUI receipt provenance mismatch')
    return proof
