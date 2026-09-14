from pathlib import Path
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
from gui_attribution import attribute, blocker_summary, classify_observations, verify_binding

HEAD_SHA, BASE_SHA, CONTROL_SHA = 'a' * 40, 'b' * 40, 'c' * 40


def binding():
    return {'pr_number': 1, 'head_sha': HEAD_SHA, 'base_sha': BASE_SHA}


def receipt(sha, pr_number, outcome='fail', procedure='hosted-gui-interaction/7', policy='v1', control=CONTROL_SHA):
    return {'policy_version': policy, 'outcome': outcome,
            'request': {'sha': sha, 'pr_number': pr_number, 'procedure_version': procedure, 'control_sha': control}}


class VerifyBindingTests(unittest.TestCase):
    def test_matching_binding_is_accepted(self):
        self.assertTrue(verify_binding(binding(), receipt(HEAD_SHA, 1), receipt(BASE_SHA, None)))

    def test_head_base_sha_must_differ(self):
        with self.assertRaises(ValueError):
            verify_binding(dict(binding(), base_sha=HEAD_SHA), receipt(HEAD_SHA, 1), receipt(HEAD_SHA, None))

    def test_missing_required_fields_rejected(self):
        for field in ('pr_number', 'head_sha', 'base_sha'):
            bad = dict(binding())
            del bad[field]
            with self.assertRaises(ValueError):
                verify_binding(bad, receipt(HEAD_SHA, 1), receipt(BASE_SHA, None))

    def test_head_receipt_must_match_head_sha_and_pr_number(self):
        with self.assertRaises(ValueError):
            verify_binding(binding(), receipt('d' * 40, 1), receipt(BASE_SHA, None))
        with self.assertRaises(ValueError):
            verify_binding(binding(), receipt(HEAD_SHA, 2), receipt(BASE_SHA, None))

    def test_baseline_must_be_anchored_to_the_current_base_sha(self):
        with self.assertRaises(ValueError):
            verify_binding(binding(), receipt(HEAD_SHA, 1), receipt('d' * 40, None))

    def test_baseline_must_share_control_procedure_and_policy(self):
        with self.assertRaises(ValueError):
            verify_binding(binding(), receipt(HEAD_SHA, 1), receipt(BASE_SHA, None, control='e' * 40))
        with self.assertRaises(ValueError):
            verify_binding(binding(), receipt(HEAD_SHA, 1), receipt(BASE_SHA, None, procedure='hosted-gui-interaction/6'))
        with self.assertRaises(ValueError):
            verify_binding(binding(), receipt(HEAD_SHA, 1), receipt(BASE_SHA, None, policy='v2'))

    def test_baseline_must_be_a_terminal_outcome(self):
        with self.assertRaises(ValueError):
            verify_binding(binding(), receipt(HEAD_SHA, 1), receipt(BASE_SHA, None, outcome='pending'))

    def test_non_mapping_receipts_rejected(self):
        with self.assertRaises(ValueError):
            verify_binding(binding(), None, receipt(BASE_SHA, None))
        with self.assertRaises(ValueError):
            verify_binding(binding(), receipt(HEAD_SHA, 1), None)


class ClassifyObservationsTests(unittest.TestCase):
    def test_identical_status_and_signature_is_pre_existing_independent(self):
        head = [{'scenario': 'inline_syntax_boundary', 'unit': 'bold_close', 'status': 'blocked', 'signature': {'actual': 70, 'canonical': 67}}]
        baseline = [dict(head[0])]
        result = classify_observations(head, baseline)
        self.assertEqual(result[0]['classification'], 'pre-existing-independent')
        self.assertEqual(blocker_summary(result), {'blocker_count': 0, 'pre_existing_independent_count': 1, 'blockers': []})

    def test_unit_the_baseline_never_exercised_is_unknown(self):
        head = [{'scenario': 'inline_syntax_boundary', 'unit': 'drag_select_delete_undo_redo_check', 'status': 'fail', 'signature': 'x'}]
        result = classify_observations(head, [])
        self.assertEqual(result[0]['classification'], 'unknown')
        self.assertEqual(blocker_summary(result)['blocker_count'], 1)

    def test_status_change_is_unknown(self):
        head = [{'scenario': 's', 'unit': 'u', 'status': 'fail', 'signature': 'x'}]
        baseline = [{'scenario': 's', 'unit': 'u', 'status': 'blocked', 'signature': 'x'}]
        self.assertEqual(classify_observations(head, baseline)[0]['classification'], 'unknown')

    def test_signature_change_is_unknown(self):
        head = [{'scenario': 's', 'unit': 'ime', 'status': 'fail',
                 'signature': 'IME confirmation produces wrong content'}]
        baseline = [{'scenario': 's', 'unit': 'ime', 'status': 'fail',
                     'signature': 'insert succeeds but undo does not restore'}]
        self.assertEqual(classify_observations(head, baseline)[0]['classification'], 'unknown')

    def test_duplicate_units_in_either_side_are_rejected(self):
        one = {'scenario': 's', 'unit': 'u', 'status': 'fail', 'signature': 'x'}
        with self.assertRaises(ValueError):
            classify_observations([one, dict(one)], [])
        with self.assertRaises(ValueError):
            classify_observations([one], [one, dict(one)])

    def test_malformed_observation_fields_are_rejected(self):
        for bad in ({'unit': 'u', 'status': 'fail', 'signature': 'x'},
                    {'scenario': 's', 'status': 'fail', 'signature': 'x'},
                    {'scenario': 's', 'unit': 'u', 'status': 'pass', 'signature': 'x'},
                    {'scenario': 's', 'unit': 'u', 'status': 'fail'},
                    'not-a-dict'):
            with self.assertRaises(ValueError):
                classify_observations([bad], [])


class AttributeTests(unittest.TestCase):
    def setUp(self):
        self.head_receipt = receipt(HEAD_SHA, 1)
        self.baseline_receipt = receipt(BASE_SHA, None)

    def test_all_pre_existing_independent_clears_the_blocker(self):
        obs = {'scenario': 's', 'unit': 'u', 'status': 'blocked', 'signature': 'same'}
        result = attribute(binding(), self.head_receipt, self.baseline_receipt, [obs], [dict(obs)])
        self.assertEqual(result['blocker_count'], 0)
        self.assertEqual(result['binding'], binding())

    def test_any_unknown_or_regression_keeps_a_blocker(self):
        baseline_obs = {'scenario': 's', 'unit': 'u', 'status': 'blocked', 'signature': 'same'}
        regression_obs = {'scenario': 's', 'unit': 'new_case', 'status': 'fail', 'signature': 'never seen before'}
        result = attribute(binding(), self.head_receipt, self.baseline_receipt,
                           [dict(baseline_obs), regression_obs], [dict(baseline_obs)])
        self.assertEqual(result['blocker_count'], 1)
        self.assertEqual(result['blockers'][0]['unit'], 'new_case')

    def test_binding_mismatch_raises_rather_than_silently_waiving(self):
        obs = {'scenario': 's', 'unit': 'u', 'status': 'fail', 'signature': 'x'}
        tampered_binding = dict(binding(), head_sha='d' * 40)
        with self.assertRaises(ValueError):
            attribute(tampered_binding, self.head_receipt, self.baseline_receipt, [obs], [dict(obs)])


if __name__ == '__main__':
    unittest.main()
