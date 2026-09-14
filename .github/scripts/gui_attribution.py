"""GUI receipt blocker attribution: separate pre-existing independent failures
from the current PR's merge-blocking scope, without ever upgrading a raw
pass/fail/blocked outcome.

This module never reads commit statuses or GitHub APIs itself. Callers bind
it to an authenticated current-head GUI receipt (`gui_policy.authenticated_receipt`)
and an authenticated baseline GUI receipt for the PR's current base SHA, plus a
small list of per-unit observations extracted from each receipt's raw result.
Attribution can only ever narrow the current PR's blocker set when a trusted
baseline, anchored to the current base SHA and run under the identical
control SHA / procedure / policy version, shows the identical (scenario, unit,
status, signature) failure. Anything the baseline did not exercise, or shows
with a different status or signature, is `unknown` and stays a blocker
(fail closed; Issue #141 / PR #144 design review on PR #145).
"""

STATUSES = ('fail', 'blocked')


def _require_observation(observation):
    if not isinstance(observation, dict):
        raise ValueError('observation must be a mapping')
    scenario, unit, status = observation.get('scenario'), observation.get('unit'), observation.get('status')
    if not isinstance(scenario, str) or not scenario:
        raise ValueError('observation missing scenario')
    if not isinstance(unit, str) or not unit:
        raise ValueError('observation missing unit')
    if status not in STATUSES:
        raise ValueError('observation status must be fail or blocked')
    if 'signature' not in observation:
        raise ValueError('observation missing signature')
    return scenario, unit


def _index(observations):
    index = {}
    for observation in observations:
        key = _require_observation(observation)
        if key in index:
            raise ValueError('duplicate observation for the same scenario/unit')
        index[key] = observation
    return index


def verify_binding(binding, head_receipt, baseline_receipt):
    """Bind attribution to the exact head/base/control/procedure/policy.

    A baseline is only usable if it is an independent trusted run that:
    - targeted the current base SHA (not the head, not an unrelated SHA),
    - used the same trusted validation control SHA as the head run,
    - used the same procedure version and policy version as the head run.

    Any mismatch means the baseline cannot be trusted to explain the head's
    failures, so it is rejected rather than silently ignored.
    """
    required = ('pr_number', 'head_sha', 'base_sha')
    for name in required:
        if not binding.get(name):
            raise ValueError(f'attribution binding missing {name}')
    if binding['head_sha'] == binding['base_sha']:
        raise ValueError('attribution binding requires a base SHA distinct from the head SHA')
    if not isinstance(head_receipt, dict) or not isinstance(baseline_receipt, dict):
        raise ValueError('attribution requires authenticated receipts')
    head_request = head_receipt.get('request') or {}
    baseline_request = baseline_receipt.get('request') or {}
    if head_request.get('sha') != binding['head_sha'] or head_request.get('pr_number') != binding['pr_number']:
        raise ValueError('head receipt is not bound to the current PR head SHA')
    if baseline_request.get('sha') != binding['base_sha']:
        raise ValueError('baseline receipt is not anchored to the current base SHA')
    if (head_receipt.get('policy_version') != baseline_receipt.get('policy_version')
            or head_request.get('procedure_version') != baseline_request.get('procedure_version')
            or head_request.get('control_sha') != baseline_request.get('control_sha')):
        raise ValueError('baseline receipt does not share the head receipt\'s control SHA, procedure, or policy version')
    if baseline_receipt.get('outcome') not in ('pass', 'fail', 'blocked'):
        raise ValueError('baseline receipt is not a terminal GUI outcome')
    return True


def classify_observations(head_observations, baseline_observations):
    """Classify every current non-passing unit against the baseline.

    `pre-existing-independent` requires an exact (scenario, unit, status,
    signature) match in the baseline. Everything else — a unit the baseline
    never exercised, a status change (e.g. baseline blocked, head fail), or a
    signature change (the symptom itself changed) — is `unknown` and stays a
    blocker. This is deliberately stricter than a fuzzy diff: only a
    machine-verifiable identical failure can be waived (Issue #141).
    """
    baseline_index = _index(baseline_observations)
    head_index = _index(head_observations)  # also rejects duplicate head units
    results = []
    for key, observation in head_index.items():
        baseline_observation = baseline_index.get(key)
        if baseline_observation is None:
            classification = 'unknown'
        elif (baseline_observation['status'] != observation['status']
              or baseline_observation['signature'] != observation['signature']):
            classification = 'unknown'
        else:
            classification = 'pre-existing-independent'
        results.append(dict(observation, classification=classification))
    return sorted(results, key=lambda item: (item['scenario'], item['unit']))


def blocker_summary(classified_observations):
    blockers = [o for o in classified_observations if o['classification'] != 'pre-existing-independent']
    return {
        'blocker_count': len(blockers),
        'pre_existing_independent_count': len(classified_observations) - len(blockers),
        'blockers': blockers,
    }


def attribute(binding, head_receipt, baseline_receipt, head_observations, baseline_observations):
    """Full attribution result: binding verification plus classified observations.

    Raises on any binding mismatch or malformed observation; never returns a
    result for an untrusted or stale comparison. `blocker_count == 0` is the
    only signal a consumer (e.g. `final_policy.gate`) may use to lift a
    scoped GUI blocker, and only together with the returned `binding` matching
    the consumer's own current exact head/PR number.
    """
    verify_binding(binding, head_receipt, baseline_receipt)
    classified = classify_observations(head_observations, baseline_observations)
    summary = blocker_summary(classified)
    return {'binding': dict(binding), 'observations': classified, **summary}
