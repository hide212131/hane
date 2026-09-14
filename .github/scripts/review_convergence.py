"""Root-cause cluster classification and convergence signals for the review/fix loop.

Issue #141: successive `finding -> fix -> new exact head -> fresh review` cycles on
the same root-cause cluster amplify the loop instead of converging it. This module
gives the pre-GUI routing / final judge decision a fixed, fail-closed shape to
classify *current* findings by root-cause cluster (not one fix per comment), and
gives the process observable signals to recognize when a cluster needs a design
review instead of another small patch.

Pure functions only: no network, no token, no GitHub API calls. Callers own
fetching commits/statuses and pass already-materialized data in.
"""
import json

CLASSIFICATIONS = ('blocker', 'follow_up', 'unknown')
# AGENTS.md: merge blocker = P0/P1, security/data-destruction/privilege-escalation,
# a clearly-reproducible normal-path defect, CI failure, or a P2 that directly
# blocks the Issue's acceptance criteria. Everything else is a follow-up
# candidate. `unknown` exists so an ambiguous finding fails closed as a blocker
# instead of being silently waived (Issue #141 / PR #145 review).
SEVERITIES = ('P0', 'P1', 'P2', 'P3')


def parse_cluster_decision(text):
    """Strict schema for a root-cause cluster classification.

    Mirrors the fail-closed style of `final_policy.parse_decision`: any shape
    mismatch raises instead of guessing a safe default, because guessing here
    would let a malformed decision silently waive a real blocker.
    """
    value = json.loads(text)
    if not isinstance(value, dict) or set(value) != {'clusters'} or not isinstance(value['clusters'], list) or not value['clusters']:
        raise ValueError('invalid cluster decision shape')
    seen_cluster_ids = set()
    seen_finding_ids = set()
    for cluster in value['clusters']:
        if not isinstance(cluster, dict) or set(cluster) != {'cluster_id', 'root_cause', 'classification', 'finding_ids'}:
            raise ValueError('invalid cluster entry shape')
        cluster_id = cluster['cluster_id']
        if not isinstance(cluster_id, str) or not cluster_id or cluster_id in seen_cluster_ids:
            raise ValueError('invalid or duplicate cluster_id')
        seen_cluster_ids.add(cluster_id)
        if not isinstance(cluster['root_cause'], str) or not 1 <= len(cluster['root_cause']) <= 2000:
            raise ValueError('invalid root_cause')
        if cluster['classification'] not in CLASSIFICATIONS:
            raise ValueError('invalid classification')
        finding_ids = cluster['finding_ids']
        if not isinstance(finding_ids, list) or not finding_ids:
            raise ValueError('cluster has no findings')
        for finding_id in finding_ids:
            if not isinstance(finding_id, str) or not finding_id or finding_id in seen_finding_ids:
                raise ValueError('invalid or duplicate finding_id')
            seen_finding_ids.add(finding_id)
    return value


def blocking_finding_ids(decision):
    """`unknown` fails closed as blocking, same as an explicit `blocker`."""
    return sorted({
        finding_id
        for cluster in decision['clusters']
        for finding_id in cluster['finding_ids']
        if cluster['classification'] in ('blocker', 'unknown')
    })


def cluster_count(decision):
    return len(decision['clusters'])


def blocker_cluster_count(decision):
    return sum(1 for cluster in decision['clusters'] if cluster['classification'] in ('blocker', 'unknown'))


def should_escalate_design_review(cluster_history, min_repeat=2):
    """A same-root-cause cluster surviving repeated fix cycles as `blocker` is a
    signal to raise fix granularity from code patch to design review, not a
    reason to keep patching or to silently give up (Issue #141 / PR #139 事後分析).

    `cluster_history` is an ordered (oldest first) sequence of per-cycle cluster
    decisions already restricted to the *current* exact head's lineage; callers
    must not mix in stale/superseded-head history themselves. Each entry is
    `{"cycle": <fix cycle index>, "root_cause": <str>, "classification": <str>}`
    for one cluster observed in that cycle. Matching is by `root_cause` text
    exactly, since `cluster_id` is not guaranteed stable across independently
    generated decisions.
    """
    streak = {}
    escalate = set()
    for entry in cluster_history:
        root_cause = entry['root_cause']
        if entry['classification'] == 'blocker':
            streak[root_cause] = streak.get(root_cause, 0) + 1
            if streak[root_cause] >= min_repeat:
                escalate.add(root_cause)
        else:
            streak[root_cause] = 0
    return sorted(escalate)


def _latest_by_context(statuses):
    latest = {}
    for status in statuses:
        context = status.get('context')
        if isinstance(status.get('id'), int) and status['id'] > latest.get(context, {}).get('id', -1):
            latest[context] = status
    return latest


def exact_head_review_count(statuses, sha):
    """How many times the *current* exact head SHA itself has been (re-)reviewed.

    Repeated `hane/codex-review` terminal results recorded for the same head
    signal a focused-review pile-up on one commit rather than the normal
    fix -> new head -> fresh review progression (Issue #141 改善方針3).
    """
    short = sha[:12]
    return sum(
        1 for status in statuses
        if status.get('context') == 'hane/codex-review'
        and status.get('state') in ('success', 'failure')
        and isinstance(status.get('description'), str)
        and status['description'].endswith(f' for {short}')
    )


def outdated_review_count(commits, statuses_by_sha, current_sha):
    """Terminal Codex reviews recorded for prior heads of this PR, now superseded.

    These belong to history, not to the current blocker set (Issue #141
    改善方針7: current-state と履歴の分離).
    """
    count = 0
    for commit in commits:
        sha = commit['sha']
        if sha == current_sha:
            continue
        latest = _latest_by_context(statuses_by_sha.get(sha, []))
        review = latest.get('hane/codex-review', {})
        if review.get('state') in ('success', 'failure') and isinstance(review.get('description'), str) \
                and review['description'].endswith(f' for {sha[:12]}'):
            count += 1
    return count


def convergence_signals(commits, statuses_by_sha, current_sha, fix_cycle_budget, cluster_decision=None):
    """Assemble the Issue #141 収束性シグナル for one PR at its current exact head.

    `fix_cycle_budget` is the result of `claude_fix_state.cycle_budget(...)`
    (already-tracked completed-fix count); this module does not duplicate that
    computation. `cluster_decision` is the current head's parsed cluster
    classification (`parse_cluster_decision` output), or `None` if none has
    been produced yet for this head.
    """
    current_statuses = statuses_by_sha.get(current_sha, [])
    return {
        'fix_cycle_count': fix_cycle_budget['completed'],
        'exact_head_review_count': exact_head_review_count(current_statuses, current_sha),
        'current_blocker_count': len(blocking_finding_ids(cluster_decision)) if cluster_decision else None,
        'cluster_count': cluster_count(cluster_decision) if cluster_decision else None,
        'blocker_cluster_count': blocker_cluster_count(cluster_decision) if cluster_decision else None,
        'outdated_review_count': outdated_review_count(commits, statuses_by_sha, current_sha),
        'commit_count': len(commits),
    }
