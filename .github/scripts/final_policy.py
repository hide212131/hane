"""Deterministic final gate. Copilot may recommend, but cannot waive evidence."""
import hashlib
import json
import re
from gui_policy import authenticated_receipt
from pipeline_api import CI_NAMES

CONTEXT = 'hane/final-judge'
AUTO_LABEL = 'agentic-auto-merge'
JUDGE_PROCEDURE = 'copilot-final/4'
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


def _gui_attribution_clears(snapshot):
    """Additive, opt-in narrowing of the GUI blocker (Issue #141).

    The raw `gui_receipt.outcome` is never rewritten. A non-pass outcome
    stays a blocker unless `gui_attribution` is present, is bound to this
    exact PR/head SHA, and reports zero unresolved blockers — i.e. every
    non-passing unit was classified `pre-existing-independent` against a
    trusted baseline anchored to the current base SHA
    (`gui_attribution.attribute`). Missing or mismatched attribution is
    fail closed: it changes nothing, preserving prior behavior.
    """
    attribution = snapshot.get('gui_attribution')
    if not isinstance(attribution, dict):
        return False
    binding = attribution.get('binding') or {}
    return (binding.get('pr_number') == snapshot.get('pr_number')
            and binding.get('head_sha') == snapshot.get('sha')
            and attribution.get('blocker_count') == 0)


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
        if proof.get('outcome') != 'pass' and not _gui_attribution_clears(snapshot):
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
