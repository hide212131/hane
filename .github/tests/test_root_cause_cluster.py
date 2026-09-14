"""Fixed root-cause cluster schema: blocker/follow_up/unknown partitioning."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import root_cause_cluster as rcc


def cluster(category, kind='gui:scenario', name='os_scroll'):
    return {'kind': kind, 'name': name, 'category': category, 'reason': 'test'}


class ValidateClusterTests(unittest.TestCase):
    def test_valid_cluster_round_trips(self):
        value = cluster('blocker')
        self.assertEqual(rcc.validate_cluster(value), value)

    def test_missing_or_invalid_fields_are_rejected(self):
        base = cluster('blocker')
        for mutation in (
            {k: v for k, v in base.items() if k != 'reason'},
            dict(base, category='waived'),
            dict(base, reason=''),
            dict(base, kind=''),
            dict(base, name=123),
        ):
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                rcc.validate_cluster(mutation)
        with self.assertRaises(ValueError):
            rcc.validate_cluster('not a dict')


class FromGuiAttributionTests(unittest.TestCase):
    def test_classification_mapping_is_fixed_and_never_invents_follow_up(self):
        attribution = {'clusters': [
            {'kind': 'scenario', 'name': 'coordinate_independent_probe', 'classification': 'target_acceptance'},
            {'kind': 'scenario', 'name': 'os_scroll', 'classification': 'regression'},
            {'kind': 'scenario', 'name': 'japanese_ime_input', 'classification': 'unknown'},
            {'kind': 'scenario', 'name': 'inline_syntax_boundary', 'classification': 'pre_existing_independent'},
            {'kind': 'top_level', 'name': 'build', 'classification': 'something_unrecognized'},
        ]}
        clusters = rcc.from_gui_attribution(attribution)
        by_name = {c['name']: c['category'] for c in clusters}
        self.assertEqual(by_name['coordinate_independent_probe'], 'blocker')
        self.assertEqual(by_name['os_scroll'], 'blocker')
        self.assertEqual(by_name['japanese_ime_input'], 'unknown')
        self.assertEqual(by_name['inline_syntax_boundary'], 'follow_up')
        # An unrecognized classification must fail closed to 'unknown', never
        # to 'follow_up', even if a future classification value is added
        # without updating this mapping.
        self.assertEqual(by_name['build'], 'unknown')

    def test_missing_or_empty_attribution_produces_no_clusters(self):
        self.assertEqual(rcc.from_gui_attribution(None), [])
        self.assertEqual(rcc.from_gui_attribution({}), [])
        self.assertEqual(rcc.from_gui_attribution({'clusters': []}), [])


class PartitionTests(unittest.TestCase):
    def test_blockers_include_unknown_but_not_follow_up(self):
        clusters = [cluster('blocker', name='a'), cluster('unknown', name='b'), cluster('follow_up', name='c')]
        self.assertEqual({c['name'] for c in rcc.blockers(clusters)}, {'a', 'b'})
        self.assertEqual({c['name'] for c in rcc.follow_ups(clusters)}, {'c'})


class SignatureTests(unittest.TestCase):
    def test_no_blockers_has_no_signature(self):
        self.assertIsNone(rcc.signature([cluster('follow_up')]))
        self.assertIsNone(rcc.signature([]))

    def test_signature_is_stable_and_order_independent(self):
        a = [cluster('blocker', name='a'), cluster('unknown', name='b')]
        b = [cluster('unknown', name='b'), cluster('blocker', name='a')]
        self.assertEqual(rcc.signature(a), rcc.signature(b))

    def test_signature_changes_when_the_blocking_set_changes(self):
        base = rcc.signature([cluster('blocker', name='a')])
        self.assertNotEqual(base, rcc.signature([cluster('blocker', name='a'), cluster('blocker', name='c')]))
        self.assertNotEqual(base, rcc.signature([cluster('blocker', name='z')]))
        # A cluster that only differs by category (still blocking) changes the signature too.
        self.assertNotEqual(base, rcc.signature([cluster('unknown', name='a')]))


if __name__ == '__main__':
    unittest.main()
