"""Issue #141: root-cause cluster classification and convergence signals must
fail closed on malformed/ambiguous input and must only escalate to a design
review on a genuine repeated-cluster streak, not on the first occurrence or on
unrelated clusters."""
from pathlib import Path
import json
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
from review_convergence import (
    blocker_cluster_count, blocking_finding_ids, cluster_count, convergence_signals,
    exact_head_review_count, outdated_review_count, parse_cluster_decision,
    should_escalate_design_review,
)

SHA = 'a' * 40
SHORT = SHA[:12]


def cluster(cluster_id, classification, finding_ids, root_cause='cause'):
    return {'cluster_id': cluster_id, 'root_cause': root_cause, 'classification': classification,
            'finding_ids': finding_ids}


def decision(*clusters):
    return {'clusters': list(clusters)}


class ParseClusterDecisionTests(unittest.TestCase):
    def test_valid_decision_round_trips(self):
        raw = json.dumps(decision(cluster('c1', 'blocker', ['f1', 'f2'])))
        parsed = parse_cluster_decision(raw)
        self.assertEqual(parsed['clusters'][0]['cluster_id'], 'c1')

    def test_rejects_extra_or_missing_top_level_keys(self):
        with self.assertRaises(ValueError):
            parse_cluster_decision(json.dumps({'clusters': [], 'extra': 1}))
        with self.assertRaises(ValueError):
            parse_cluster_decision(json.dumps({}))

    def test_rejects_empty_clusters(self):
        with self.assertRaises(ValueError):
            parse_cluster_decision(json.dumps({'clusters': []}))

    def test_rejects_unknown_classification(self):
        raw = json.dumps(decision(cluster('c1', 'waived', ['f1'])))
        with self.assertRaises(ValueError):
            parse_cluster_decision(raw)

    def test_rejects_duplicate_cluster_id(self):
        raw = json.dumps(decision(cluster('c1', 'blocker', ['f1']), cluster('c1', 'follow_up', ['f2'])))
        with self.assertRaises(ValueError):
            parse_cluster_decision(raw)

    def test_rejects_finding_id_assigned_to_two_clusters(self):
        raw = json.dumps(decision(cluster('c1', 'blocker', ['f1']), cluster('c2', 'follow_up', ['f1'])))
        with self.assertRaises(ValueError):
            parse_cluster_decision(raw)

    def test_rejects_cluster_with_no_findings(self):
        raw = json.dumps(decision(cluster('c1', 'blocker', [])))
        with self.assertRaises(ValueError):
            parse_cluster_decision(raw)

    def test_rejects_malformed_cluster_entry_shape(self):
        raw = json.dumps({'clusters': [{'cluster_id': 'c1', 'classification': 'blocker', 'finding_ids': ['f1']}]})
        with self.assertRaises(ValueError):
            parse_cluster_decision(raw)


class BlockingFindingIdsTests(unittest.TestCase):
    def test_blocker_and_unknown_are_blocking_follow_up_is_not(self):
        data = decision(
            cluster('c1', 'blocker', ['f1']),
            cluster('c2', 'follow_up', ['f2']),
            cluster('c3', 'unknown', ['f3']),
        )
        self.assertEqual(blocking_finding_ids(data), ['f1', 'f3'])

    def test_cluster_and_blocker_cluster_counts(self):
        data = decision(
            cluster('c1', 'blocker', ['f1']),
            cluster('c2', 'follow_up', ['f2']),
            cluster('c3', 'unknown', ['f3']),
        )
        self.assertEqual(cluster_count(data), 3)
        self.assertEqual(blocker_cluster_count(data), 2)


class EscalationTests(unittest.TestCase):
    def test_no_escalation_on_first_occurrence(self):
        history = [{'cycle': 1, 'root_cause': 'evidence contract', 'classification': 'blocker'}]
        self.assertEqual(should_escalate_design_review(history), [])

    def test_escalates_on_repeated_blocker_in_same_cluster(self):
        history = [
            {'cycle': 1, 'root_cause': 'evidence contract', 'classification': 'blocker'},
            {'cycle': 2, 'root_cause': 'evidence contract', 'classification': 'blocker'},
        ]
        self.assertEqual(should_escalate_design_review(history), ['evidence contract'])

    def test_resolved_cycle_resets_the_streak(self):
        history = [
            {'cycle': 1, 'root_cause': 'evidence contract', 'classification': 'blocker'},
            {'cycle': 2, 'root_cause': 'evidence contract', 'classification': 'follow_up'},
            {'cycle': 3, 'root_cause': 'evidence contract', 'classification': 'blocker'},
        ]
        self.assertEqual(should_escalate_design_review(history), [])

    def test_different_clusters_do_not_combine(self):
        history = [
            {'cycle': 1, 'root_cause': 'evidence contract', 'classification': 'blocker'},
            {'cycle': 1, 'root_cause': 'IME validation', 'classification': 'blocker'},
        ]
        self.assertEqual(should_escalate_design_review(history), [])

    def test_min_repeat_is_configurable(self):
        history = [{'cycle': i, 'root_cause': 'x', 'classification': 'blocker'} for i in range(1, 4)]
        self.assertEqual(should_escalate_design_review(history, min_repeat=3), ['x'])
        self.assertEqual(should_escalate_design_review(history, min_repeat=4), [])


def status(id, context, state, description):
    return {'id': id, 'context': context, 'state': state, 'description': description}


class ExactHeadReviewCountTests(unittest.TestCase):
    def test_counts_terminal_reviews_for_this_sha_only(self):
        statuses = [
            status(1, 'hane/codex-review', 'failure', f'Codex findings for {SHORT}'),
            status(2, 'hane/codex-review', 'success', f'Codex review clean for {SHORT}'),
            status(3, 'hane/copilot-routing', 'success', f'Copilot routing: fix for {SHORT}'),
        ]
        self.assertEqual(exact_head_review_count(statuses, SHA), 2)

    def test_ignores_pending_and_other_sha(self):
        other = 'b' * 40
        statuses = [
            status(1, 'hane/codex-review', 'pending', f'Codex review pending for {SHORT}'),
            status(2, 'hane/codex-review', 'failure', f'Codex findings for {other[:12]}'),
        ]
        self.assertEqual(exact_head_review_count(statuses, SHA), 0)


class OutdatedReviewCountTests(unittest.TestCase):
    def test_counts_terminal_reviews_on_prior_heads_only(self):
        commits = [{'sha': 'a' * 40}, {'sha': 'b' * 40}, {'sha': 'c' * 40}]
        statuses_by_sha = {
            'a' * 40: [status(1, 'hane/codex-review', 'failure', f"Codex findings for {'a' * 12}")],
            'b' * 40: [status(2, 'hane/codex-review', 'success', f"Codex review clean for {'b' * 12}")],
            'c' * 40: [status(3, 'hane/codex-review', 'success', f"Codex review clean for {'c' * 12}")],
        }
        self.assertEqual(outdated_review_count(commits, statuses_by_sha, current_sha='c' * 40), 2)

    def test_current_head_never_counts_as_outdated(self):
        commits = [{'sha': SHA}]
        statuses_by_sha = {SHA: [status(1, 'hane/codex-review', 'success', f'Codex review clean for {SHORT}')]}
        self.assertEqual(outdated_review_count(commits, statuses_by_sha, current_sha=SHA), 0)


class ConvergenceSignalsTests(unittest.TestCase):
    def test_assembles_all_signals_with_a_current_decision(self):
        commits = [{'sha': 'a' * 40}, {'sha': SHA}]
        statuses_by_sha = {
            'a' * 40: [status(1, 'hane/codex-review', 'failure', f"Codex findings for {'a' * 12}")],
            SHA: [status(2, 'hane/codex-review', 'failure', f'Codex findings for {SHORT}')],
        }
        current_decision = decision(cluster('c1', 'blocker', ['f1']), cluster('c2', 'follow_up', ['f2']))
        signals = convergence_signals(
            commits, statuses_by_sha, current_sha=SHA,
            fix_cycle_budget={'completed': 2, 'boundary_sha': ''}, cluster_decision=current_decision,
        )
        self.assertEqual(signals, {
            'fix_cycle_count': 2, 'exact_head_review_count': 1, 'current_blocker_count': 1,
            'cluster_count': 2, 'blocker_cluster_count': 1, 'outdated_review_count': 1, 'commit_count': 2,
        })

    def test_missing_cluster_decision_reports_none_not_zero(self):
        # `None` (no classification produced yet for this head) must stay
        # distinguishable from `0` (classified and confirmed empty), so a
        # caller cannot mistake "not yet classified" for "no blockers".
        signals = convergence_signals(
            [{'sha': SHA}], {SHA: []}, current_sha=SHA,
            fix_cycle_budget={'completed': 0, 'boundary_sha': ''}, cluster_decision=None,
        )
        self.assertIsNone(signals['current_blocker_count'])
        self.assertIsNone(signals['cluster_count'])
        self.assertIsNone(signals['blocker_cluster_count'])


if __name__ == '__main__':
    unittest.main()
