import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import aadw_workflow_run_gate as gate

REPO = 'hide212131/hane'


class Fake:
    def __init__(self, conclusion='skipped'):
        self.conclusion = conclusion
        self.calls = []

    def call(self, endpoint):
        self.calls.append(endpoint)
        return {'jobs': [
            {'name': 'authorize', 'conclusion': 'success'},
            {'name': 'implement', 'conclusion': self.conclusion},
        ]}


def payload(*, name='Implement issue with Claude', event='issue_comment', run_id=123, attempt=2):
    return {'workflow_run': {
        'id': run_id,
        'run_attempt': attempt,
        'name': name,
        'event': event,
    }}


class Tests(unittest.TestCase):
    def test_irrelevant_issue_comment_run_is_ignored_when_implement_job_was_skipped(self):
        fake = Fake('skipped')
        self.assertFalse(gate.should_observe(fake.call, REPO, payload()))
        self.assertEqual(
            fake.calls,
            [f'repos/{REPO}/actions/runs/123/attempts/2/jobs?per_page=100'],
        )

    def test_actual_failed_implement_job_is_observed(self):
        fake = Fake('failure')
        self.assertTrue(gate.should_observe(fake.call, REPO, payload()))

    def test_cancelled_implement_job_is_observed_as_real_execution(self):
        fake = Fake('cancelled')
        self.assertTrue(gate.should_observe(fake.call, REPO, payload()))

    def test_other_workflow_run_is_not_filtered(self):
        fake = Fake('skipped')
        self.assertTrue(gate.should_observe(
            fake.call, REPO, payload(name='Codex review gate')
        ))
        self.assertEqual(fake.calls, [])

    def test_non_workflow_run_event_is_not_filtered(self):
        fake = Fake('skipped')
        self.assertTrue(gate.should_observe(fake.call, REPO, {'action': 'created'}))
        self.assertEqual(fake.calls, [])

    def test_missing_implement_job_fails_closed(self):
        def call(_endpoint):
            return {'jobs': [{'name': 'authorize', 'conclusion': 'success'}]}
        with self.assertRaises(RuntimeError):
            gate.should_observe(call, REPO, payload())


if __name__ == '__main__':
    unittest.main()
