"""Fixed root-cause cluster schema shared by trusted current-head findings.

`category` is exactly one of:

- ``blocker``: keeps the current Pull Request blocked. A fix is required, or
  the evidence needed to clear it fail-closed is still missing.
- ``follow_up``: does not block the current Pull Request by itself. Recorded
  so a single finding does not force a fix cycle when the current Pull
  Request's own acceptance evidence is otherwise machine-verified complete.
- ``unknown``: evidence is missing, ambiguous, stale, or fails a fail-closed
  binding check. Treated exactly like ``blocker`` everywhere except its
  label; it must never silently become ``follow_up``.

Only GUI scenario/step failures, classified by
``gui_baseline_attribution.attribute()`` against an authenticated baseline,
are mapped through this schema today. Review-comment and CI-finding
clustering need their own trusted-controller generation step (see Issue
#147/#141 follow-up) before they can safely feed this same fixed schema.
"""
import hashlib
import json

CONTEXT = 'hane/root-cause-signature'
CATEGORIES = ('blocker', 'follow_up', 'unknown')

# gui_baseline_attribution.attribute() classifications, mapped onto the fixed
# schema. 'pre_existing_independent' is the only classification that may ever
# leave the current Pull Request's blocker set; everything else keeps
# blocking (as 'blocker'), or keeps blocking while being labeled 'unknown' so
# it is never confused with a proven-safe waiver.
GUI_CLASSIFICATION_CATEGORY = {
    'target_acceptance': 'blocker',
    'regression': 'blocker',
    'unknown': 'unknown',
    'pre_existing_independent': 'follow_up',
}


def validate_cluster(cluster):
    if not isinstance(cluster, dict):
        raise ValueError('cluster must be an object')
    required = {'kind', 'name', 'category', 'reason'}
    if not required <= set(cluster):
        raise ValueError('cluster missing required fields')
    if not isinstance(cluster['kind'], str) or not cluster['kind']:
        raise ValueError('cluster kind invalid')
    if not isinstance(cluster['name'], str) or not cluster['name']:
        raise ValueError('cluster name invalid')
    if cluster['category'] not in CATEGORIES:
        raise ValueError('cluster category invalid')
    if not isinstance(cluster['reason'], str) or not cluster['reason']:
        raise ValueError('cluster reason invalid')
    return cluster


def from_gui_attribution(attribution):
    """Deterministically derive fixed-schema clusters from an already-computed
    gui_baseline_attribution.attribute() result.

    Never trusts free-form input: every category is a pure function of the
    machine-verified classification already computed against authenticated
    receipts. An unrecognized or missing classification maps to 'unknown',
    never to 'follow_up'.
    """
    clusters = []
    for unit in (attribution or {}).get('clusters', []):
        classification = unit.get('classification')
        category = GUI_CLASSIFICATION_CATEGORY.get(classification, 'unknown')
        clusters.append(validate_cluster({
            'kind': f'gui:{unit.get("kind")}', 'name': unit.get('name'),
            'category': category,
            'reason': f'GUI baseline attribution classified this failure as {classification}',
        }))
    return clusters


def blockers(clusters):
    return [c for c in clusters if c['category'] in ('blocker', 'unknown')]


def follow_ups(clusters):
    return [c for c in clusters if c['category'] == 'follow_up']


def signature(clusters):
    """Stable identity of the current blocking cluster set, independent of the
    exact head SHA. Used to detect the same root cause repeating across
    fix cycles. Returns None when there is nothing blocking to sign."""
    blocking = sorted((c['kind'], c['name'], c['category']) for c in blockers(clusters))
    if not blocking:
        return None
    return hashlib.sha256(json.dumps(blocking, sort_keys=True, separators=(',', ':')).encode()).hexdigest()[:16]
