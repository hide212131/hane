from pathlib import Path
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
from review_convergence import classify_clusters, cluster_findings, convergence_signals, design_review_escalations


def finding(id, cluster, severity, **extra):
    return {'id': id, 'cluster': cluster, 'severity': severity, **extra}


class ClusterFindingsTests(unittest.TestCase):
    def test_groups_by_explicit_cluster_tag(self):
        findings = [finding('f1', 'evidence-contract', 'P1'), finding('f2', 'evidence-contract', 'P2'),
                    finding('f3', 'ime-validation', 'P1')]
        clusters = cluster_findings(findings)
        self.assertEqual(set(clusters), {'evidence-contract', 'ime-validation'})
        self.assertEqual(len(clusters['evidence-contract']), 2)

    def test_missing_cluster_tag_is_rejected_rather_than_becoming_a_singleton(self):
        with self.assertRaises(ValueError):
            cluster_findings([{'id': 'f1', 'severity': 'P1'}])

    def test_missing_id_or_invalid_severity_is_rejected(self):
        with self.assertRaises(ValueError):
            cluster_findings([{'cluster': 'c', 'severity': 'P1'}])
        with self.assertRaises(ValueError):
            cluster_findings([finding('f1', 'c', 'P9')])

    def test_non_mapping_finding_is_rejected(self):
        with self.assertRaises(ValueError):
            cluster_findings(['not-a-finding'])


class ClassifyClustersTests(unittest.TestCase):
    def test_p0_and_p1_are_always_blockers(self):
        for severity in ('P0', 'P1'):
            clusters = cluster_findings([finding('f1', 'c', severity)])
            result = classify_clusters(clusters)
            self.assertEqual(result['c']['status'], 'blocker')
            self.assertEqual(result['c']['blocking_findings'], clusters['c'])

    def test_p2_is_a_blocker_only_when_it_blocks_acceptance(self):
        blocking = cluster_findings([finding('f1', 'c', 'P2', blocks_acceptance=True)])
        self.assertEqual(classify_clusters(blocking)['c']['status'], 'blocker')
        not_blocking = cluster_findings([finding('f1', 'c', 'P2')])
        self.assertEqual(classify_clusters(not_blocking)['c']['status'], 'follow-up-candidate')

    def test_p3_is_a_follow_up_candidate(self):
        clusters = cluster_findings([finding('f1', 'c', 'P3')])
        result = classify_clusters(clusters)
        self.assertEqual(result['c']['status'], 'follow-up-candidate')
        self.assertEqual(result['c']['blocking_findings'], [])

    def test_mixed_severities_in_one_cluster_are_a_blocker_if_any_member_blocks(self):
        clusters = cluster_findings([finding('f1', 'c', 'P3'), finding('f2', 'c', 'P1')])
        result = classify_clusters(clusters)
        self.assertEqual(result['c']['status'], 'blocker')
        self.assertEqual(len(result['c']['blocking_findings']), 1)


class DesignReviewEscalationTests(unittest.TestCase):
    def test_threshold_reached_escalates(self):
        self.assertEqual(design_review_escalations({'evidence-contract': 2, 'ime-validation': 1}), ['evidence-contract'])

    def test_default_threshold_is_two_cycles(self):
        self.assertEqual(design_review_escalations({'c': 1}), [])
        self.assertEqual(design_review_escalations({'c': 2}), ['c'])

    def test_custom_threshold(self):
        self.assertEqual(design_review_escalations({'c': 1}, threshold=1), ['c'])

    def test_invalid_threshold_or_counts_rejected(self):
        with self.assertRaises(ValueError):
            design_review_escalations({'c': 1}, threshold=0)
        with self.assertRaises(ValueError):
            design_review_escalations({'c': -1})


class ConvergenceSignalsTests(unittest.TestCase):
    def test_bundles_all_signals(self):
        signals = convergence_signals(fix_cycle_count=1, exact_head_review_count=2, current_blocker_cluster_count=1,
                                      cluster_count=3, outdated_review_count=4, commit_count=17)
        self.assertEqual(signals, {'fix_cycle_count': 1, 'exact_head_review_count': 2, 'current_blocker_cluster_count': 1,
                                   'cluster_count': 3, 'outdated_review_count': 4, 'commit_count': 17})

    def test_rejects_negative_or_non_integer_signals(self):
        base = dict(fix_cycle_count=1, exact_head_review_count=1, current_blocker_cluster_count=0,
                    cluster_count=1, outdated_review_count=0, commit_count=1)
        for key, value in (('fix_cycle_count', -1), ('commit_count', 'many'), ('cluster_count', True)):
            with self.assertRaises(ValueError):
                convergence_signals(**dict(base, **{key: value}))


if __name__ == '__main__':
    unittest.main()
