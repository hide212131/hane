"""Bind a current-head GUI fail outcome to a trusted baseline so a proven
pre-existing-independent scenario/step failure -- never the target scenario,
never a regression, never an unauthenticated or mismatched comparison -- can
be separated from the current Pull Request's GUI blocker.

Fail closed on anything short of an exact, authenticated, same
control-SHA/procedure/policy comparison against a baseline anchored to the
current Pull Request's base SHA, or to a nearer main-line ancestor the
controller itself confirmed is product-equivalent for GUI purposes (no
GUI-relevant file changed between that ancestor and the base SHA).

Never upgrades 'blocked' outcomes (they carry no per-scenario evidence
contract to compare against) and never reclassifies the fixed target
acceptance scenario(s), regardless of baseline agreement.
"""
from pathlib import Path
import re
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
from gui_policy import CONTEXT as GUI_CONTEXT, PROCEDURE, authenticated_receipt, gui_state, required_from_files
from gui_artifacts import artifact_json

# A current-head failure in one of these scenarios always stays a
# target-acceptance blocker, regardless of any baseline match. Extend only
# through trusted code review, never through PR/issue prose.
TARGET_ACCEPTANCE_SCENARIOS = frozenset({'coordinate_independent_probe'})
PRODUCT_EQUIVALENT_SEARCH_LIMIT = 30


def failing_units(raw):
    """Machine-derived failure signature units from an already-authenticated
    receipt's raw result. Relies on validate_receipt's fail-evidence contract
    (see gui_policy.py) having already bounded which step names can report
    'fail' for a given top-level step or scenario."""
    units = []
    for step in raw.get('top_level_steps', []) or []:
        if step.get('result') == 'fail':
            units.append({'kind': 'top_level', 'name': step.get('name'), 'steps': ()})
    for scenario in raw.get('scenarios', []) or []:
        if scenario.get('result') == 'fail':
            failing_steps = tuple(sorted({s.get('name') for s in scenario.get('steps', []) if s.get('result') == 'fail'}))
            units.append({'kind': 'scenario', 'name': scenario.get('name'), 'steps': failing_steps})
    return units


def _binding_matches(current_request, baseline_request):
    return (isinstance(current_request, dict) and isinstance(baseline_request, dict)
            and current_request.get('control_sha') == baseline_request.get('control_sha')
            and current_request.get('procedure_version') == baseline_request.get('procedure_version')
            and current_request.get('procedure_version') == PROCEDURE)


def baseline_gui_receipt(api, sha, statuses=None):
    """Authenticated GUI receipt for an arbitrary main-line sha, or None if no
    terminal GUI result is recorded for it. Recovers the originating Pull
    Request number (needed to name the receipt artifact) from the trusted
    run's own job list, since the commit status alone does not carry it."""
    statuses = api.statuses(sha) if statuses is None else statuses
    row = statuses.get(GUI_CONTEXT, {})
    state = gui_state(row, sha)
    if not state or state[0] == 'pending':
        return None
    run_id, attempt = state[1].split('-')
    if row.get('target_url') != f'https://github.com/{api.repository}/actions/runs/{run_id}':
        raise ValueError('GUI status URL mismatch')
    run = api.api(api.repo(f'actions/runs/{run_id}/attempts/{attempt}'))
    if run.get('status') != 'completed':
        return None
    jobs = api.pages(api.repo(f'actions/runs/{run_id}/attempts/{attempt}/jobs'), 'jobs')
    number = None
    for job in jobs:
        match = re.fullmatch(r'GUI worker #(\d+)', job.get('name', '') or '')
        if match:
            number = int(match[1])
            break
    if number is None:
        return None
    try:
        proof = artifact_json(api, run_id, f'gui-receipt-{number}-{state[1]}', 'gui-receipt.json')
    except (ValueError, KeyError):
        if state[0] != 'blocked':
            raise
        proof = artifact_json(api, run_id, f'gui-recovered-{state[1]}', f'{number}.json')
    return authenticated_receipt(proof, sha, number, api.repository, state, run)


def product_equivalent_baseline(api, base_sha, limit=PRODUCT_EQUIVALENT_SEARCH_LIMIT):
    """Nearest main-line ancestor of base_sha with a terminal GUI receipt whose
    diff up to base_sha touches nothing GUI-relevant. Fail closed to None on
    any ambiguity (large/omitted diff, transport error, no such ancestor)."""
    commits = api.pages(api.repo(f'commits?sha={base_sha}&per_page=100'))[:limit]
    for commit in commits[1:]:
        sha = commit.get('sha') if isinstance(commit, dict) else None
        if not sha:
            continue
        try:
            receipt = baseline_gui_receipt(api, sha)
        except (ValueError, KeyError):
            continue
        if receipt is None:
            continue
        diff = api.api(api.repo(f'compare/{sha}...{base_sha}'))
        files = diff.get('files') if isinstance(diff, dict) else None
        if files is None:
            return None
        if files:
            try:
                if required_from_files(files, len(files)):
                    return None
            except ValueError:
                return None
        return {'sha': sha, 'receipt': receipt, 'source': 'controller-confirmed-equivalent'}
    return None


def attribute(current_proof, base_sha, baseline):
    """Classify every current-head GUI failure unit.

    current_proof: an authenticated_receipt()-verified proof for the current
      head, as stored on gui_receipt.
    base_sha: the current Pull Request's base commit SHA.
    baseline: None, or {'sha', 'receipt', 'source'} where 'source' is
      'base-sha' (baseline['sha'] == base_sha) or
      'controller-confirmed-equivalent' (a nearer, GUI-relevant-change-free
      ancestor of base_sha), and 'receipt' is an authenticated_receipt()
      -verified proof for baseline['sha'].

    Returns {'baseline': {...} | None, 'clusters': [...], 'blocking': bool}.
    Never upgrades an unauthenticated, mismatched, or ambiguous comparison to
    pre_existing_independent; unmapped/uncertain cases stay 'unknown'.
    """
    result = {'baseline': None, 'clusters': [], 'blocking': True}
    outcome = current_proof.get('outcome') if isinstance(current_proof, dict) else None
    if outcome != 'fail':
        # 'pass' needs no attribution; 'blocked' has no per-scenario evidence
        # contract to compare against, so it stays a fail-closed blocker.
        return result
    raw = current_proof.get('result') or {}
    units = failing_units(raw)
    current_request = current_proof.get('request') or {}

    baseline_request = baseline_raw = None
    baseline_outcome = None
    if baseline is not None:
        baseline_receipt = baseline['receipt']
        baseline_request = baseline_receipt.get('request') or {}
        baseline_outcome = baseline_receipt.get('outcome')
        baseline_raw = baseline_receipt.get('result') or {}
        result['baseline'] = {'sha': baseline['sha'], 'source': baseline['source'], 'outcome': baseline_outcome}

    binding_ok = (baseline is not None
                  and ((baseline.get('source') == 'base-sha' and baseline.get('sha') == base_sha)
                       or baseline.get('source') == 'controller-confirmed-equivalent')
                  and baseline_outcome == 'fail' and _binding_matches(current_request, baseline_request))
    baseline_units = failing_units(baseline_raw) if binding_ok else []
    baseline_index = {(u['kind'], u['name']): u['steps'] for u in baseline_units}

    clusters = []
    blocking = False
    for unit in units:
        key = (unit['kind'], unit['name'])
        if unit['name'] in TARGET_ACCEPTANCE_SCENARIOS:
            classification = 'target_acceptance'
        elif not binding_ok:
            classification = 'unknown'
        elif key not in baseline_index:
            # Not failing at baseline (or never executed there): this is new
            # to the current head, so it is a regression, not pre-existing.
            classification = 'regression'
        elif baseline_index[key] != unit['steps']:
            # Same scenario/step name failed at baseline, but the failing
            # step signature differs: the symptom changed, so it is unknown,
            # never pre-existing.
            classification = 'unknown'
        else:
            classification = 'pre_existing_independent'
        if classification != 'pre_existing_independent':
            blocking = True
        clusters.append({'kind': unit['kind'], 'name': unit['name'],
                         'failing_steps': list(unit['steps']), 'classification': classification})
    result['clusters'] = clusters
    result['blocking'] = blocking or not units
    return result
