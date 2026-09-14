"""Root-cause cluster classification for the AADW review/fix loop (Issue #141).

Pure, fail-closed helpers. Nothing here reads GitHub state; callers supply
review findings and already-computed counters. The goal is to stop the
review/fix loop from amplifying: findings are grouped by an explicit
root-cause cluster tag before any fix is dispatched, blocker/follow-up status
is derived from severity rather than "any finding exists", and a cluster that
keeps producing new merge-blocking P0/P1 findings across fix cycles is flagged
for a design review instead of another localized patch.
"""

SEVERITIES = ('P0', 'P1', 'P2', 'P3')
BLOCKING_SEVERITIES = ('P0', 'P1')


def _require_finding(finding):
    if not isinstance(finding, dict):
        raise ValueError('finding must be a mapping')
    finding_id, cluster, severity = finding.get('id'), finding.get('cluster'), finding.get('severity')
    if not isinstance(finding_id, str) or not finding_id:
        raise ValueError('finding missing id')
    if not isinstance(cluster, str) or not cluster:
        raise ValueError('finding missing root-cause cluster tag')
    if severity not in SEVERITIES:
        raise ValueError('finding missing or invalid severity')
    return cluster


def cluster_findings(findings):
    """Group findings by their explicit root-cause cluster tag.

    A finding without a `cluster` tag is rejected rather than treated as its
    own singleton cluster: forcing the tag is what prevents "1 review
    comment = 1 fix cycle" (Issue #141, AGENTS.md の既存原則).
    """
    clusters = {}
    for finding in findings:
        cluster = _require_finding(finding)
        clusters.setdefault(cluster, []).append(finding)
    return clusters


def classify_clusters(clusters):
    """Per-cluster blocker / follow-up-candidate status.

    P0/P1 is always a merge blocker. P2 is a blocker only if the finding is
    explicitly marked `blocks_acceptance` (directly prevents the target
    Issue's acceptance criteria, per AGENTS.md). Everything else is a
    follow-up candidate, never a current-PR blocker on its own.
    """
    result = {}
    for cluster, findings in clusters.items():
        blocking = [f for f in findings
                    if f['severity'] in BLOCKING_SEVERITIES or (f['severity'] == 'P2' and f.get('blocks_acceptance') is True)]
        result[cluster] = {
            'status': 'blocker' if blocking else 'follow-up-candidate',
            'blocking_findings': blocking,
            'findings': findings,
        }
    return result


def design_review_escalations(cluster_cycle_counts, threshold=2):
    """Clusters whose repeated merge-blocking findings should escalate to a
    design review instead of another local fix cycle.

    `cluster_cycle_counts` maps a cluster tag to the number of fix cycles in
    which a *new* merge-blocking P0/P1 finding appeared in that cluster after
    a fix already targeted it. Reaching `threshold` is a switch from
    code-patch granularity to design-review granularity, not a cap that
    silently drops the problem (Issue #141 / #139 事後分析).
    """
    if not isinstance(threshold, int) or isinstance(threshold, bool) or threshold < 1:
        raise ValueError('threshold must be a positive integer')
    for cluster, count in cluster_cycle_counts.items():
        if not isinstance(count, int) or isinstance(count, bool) or count < 0:
            raise ValueError(f'cycle count for cluster {cluster!r} must be a non-negative integer')
    return sorted(cluster for cluster, count in cluster_cycle_counts.items() if count >= threshold)


def convergence_signals(*, fix_cycle_count, exact_head_review_count, current_blocker_cluster_count,
                         cluster_count, outdated_review_count, commit_count):
    """Bundle the Issue #141 収束性シグナル into one reportable snapshot.

    Pure aggregation of already-computed counters (e.g. `fix_cycle_count` can
    come from `claude_fix_state.cycle_budget`); no thresholds here trigger an
    automatic stop. This is judgement material, not a gate.
    """
    values = {'fix_cycle_count': fix_cycle_count, 'exact_head_review_count': exact_head_review_count,
              'current_blocker_cluster_count': current_blocker_cluster_count, 'cluster_count': cluster_count,
              'outdated_review_count': outdated_review_count, 'commit_count': commit_count}
    for name, value in values.items():
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            raise ValueError(f'{name} must be a non-negative integer')
    return values
