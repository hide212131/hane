"""Final-judge boundaries: terminal GUI outcomes, stale data and actual merge CAS."""
from copy import deepcopy
from pathlib import Path
import json
import os
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import final_pipeline as controller
from final_policy import authenticated_receipt, fingerprint, final_state, gate, may_judge, parse_decision
from claude_fix_state import routing_allows_fix
from pipeline_api import CI_NAMES

SHA = 'a' * 40


def ready(gui=True):
    return {'pr_number': 1, 'sha': SHA, 'repository': 'owner/repo', 'trusted': True, 'mergeable': True,
            'ci_ready': True, 'review_ready': True, 'classified': True, 'gui_required': gui,
            'unresolved_threads': [], 'blocking_reviews': [], 'required_checks': sorted(CI_NAMES),
            'workflow_changes': False, 'auto_merge': True,
            'gui_receipt': {'outcome': 'pass'} if gui else None}


class PolicyTests(unittest.TestCase):
    def test_all_gui_terminal_outcomes_reach_final_judge(self):
        for outcome in ('pass', 'fail', 'blocked'):
            data = ready()
            data['gui_receipt']['outcome'] = outcome
            self.assertTrue(may_judge(data))
            self.assertEqual(not gate(data), outcome == 'pass')
            data['ci_ready'] = False
            self.assertTrue(may_judge(data))
            self.assertTrue(gate(data))

    def test_required_missing_conditions_always_prevent_ready(self):
        cases = [('trusted', False), ('mergeable', False), ('mergeable', None), ('ci_ready', False), ('review_ready', False),
                 ('classified', False), ('unresolved_threads', ['thread']),
                 ('blocking_reviews', [12]), ('required_checks', []),
                 ('required_checks', sorted(CI_NAMES) + ['new required check']),
                 ('workflow_changes', True), ('gui_receipt', None)]
        self.assertEqual(gate(ready()), [])
        for key, value in cases:
            with self.subTest(key=key, value=value):
                data = ready()
                data[key] = value
                self.assertTrue(gate(data))
        self.assertEqual(gate(ready(gui=False)), [])

    def test_exact_json_only_no_surrounding_instructions_or_unknown_decision(self):
        for value in ('{"decision":"ready","reason":"complete"}', '{"decision":"fix","reason":"save failed"}'):
            self.assertIn(parse_decision(value)['decision'], ('ready', 'fix'))
        for value in ('```json\n{"decision":"ready","reason":"ok"}\n```', '{}',
                      '{"decision":"merge","reason":"ok"}', '{"decision":"ready","reason":""}',
                      '{"decision":"ready","reason":"ok","override":true}'):
            with self.assertRaises(ValueError):
                parse_decision(value)

    def test_receipt_must_come_from_main_exact_attempt_and_procedure_run(self):
        request = {'pr_number': 1, 'sha': SHA, 'repository': 'owner/repo', 'control_sha': 'b' * 40,
                   'generation': '123-2', 'run_id': '123', 'run_attempt': '2', 'request_id': 'gui-123-2-pr1'}
        proof = {'schema_version': 1, 'policy_version': 'v1', 'request': request, 'outcome': 'pass'}
        run = {'id': 123, 'run_attempt': 2, 'status': 'completed', 'path': '.github/workflows/gui-validation.yml',
               'head_branch': 'main', 'head_sha': 'b' * 40, 'event': 'workflow_dispatch'}
        self.assertEqual(authenticated_receipt(proof, SHA, 1, 'owner/repo', ('pass', '123-2'), run), proof)
        for field, value in [('run_attempt', 3), ('head_branch', 'untrusted'), ('path', '.github/workflows/ci.yml'),
                             ('head_sha', 'c' * 40), ('status', 'in_progress'), ('event', 'push')]:
            bad = dict(run, **{field: value})
            with self.subTest(field=field), self.assertRaises(ValueError):
                authenticated_receipt(proof, SHA, 1, 'owner/repo', ('pass', '123-2'), bad)

    def test_final_fix_authorization_can_be_superseded(self):
        final = {'id': 10, 'context': 'hane/final-judge', 'state': 'failure',
                 'description': f'Final fix v1 {SHA[:12]} e' + 'b' * 16}
        self.assertTrue(routing_allows_fix([final], SHA))
        for context, description, state in [('hane/copilot-routing', f'Copilot routing: continue-validation for {SHA[:12]}', 'success'),
                                           ('hane/final-judge', f'Final blocked v1 {SHA[:12]} e' + 'b' * 16, 'error')]:
            self.assertFalse(routing_allows_fix([final, {'id': 11, 'context': context, 'description': description, 'state': state}], SHA))


class FakeAPI:
    repository = 'owner/repo'

    def __init__(self):
        self.rows, self.writes, self.merges = {}, [], []

    def statuses(self, sha):
        return self.rows

    def post_status(self, sha, context, state, description):
        self.writes.append((sha, context, state, description))
        self.rows[context] = {'state': state, 'description': description}

    def repo(self, path):
        return path

    def api(self, path, body=None, method=None):
        self.merges.append((path, body, method))
        return {'merged': True, 'sha': 'c' * 40}


class EffectTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name)
        self.env = patch.dict(os.environ, {'GITHUB_RUN_ID': '123', 'GITHUB_RUN_ATTEMPT': '1'})
        self.env.start()
        self.addCleanup(self.env.stop)

    def test_ready_opt_in_merges_with_exact_sha_once(self):
        api, data = FakeAPI(), ready()
        with patch.object(controller, 'snapshot', return_value=data), patch.object(controller, 'judge', return_value={'decision': 'ready', 'reason': 'all passed'}) as judge:
            controller.process(api, 1, self.directory)
            controller.process(api, 1, self.directory)
        self.assertEqual(judge.call_count, 1)
        self.assertEqual(api.merges, [('pulls/1/merge', {'sha': SHA, 'merge_method': 'squash'}, 'PUT')])
        self.assertEqual(json.loads((self.directory / '1.json').read_text())['effect'], 'merged')

    def test_no_opt_in_and_any_denial_prevent_merge_even_if_judge_says_ready(self):
        cases = [('auto_merge', False), ('ci_ready', False), ('workflow_changes', True), ('unresolved_threads', ['x']),
                 ('gui_receipt', {'outcome': 'fail'}), ('gui_receipt', {'outcome': 'blocked'})]
        for field, value in cases:
            api, data = FakeAPI(), ready()
            data[field] = value
            with self.subTest(field=field, value=value), patch.object(controller, 'snapshot', return_value=data), patch.object(controller, 'judge', return_value={'decision': 'ready', 'reason': 'model recommendation'}):
                controller.process(api, 1, self.directory)
                self.assertEqual(api.merges, [])

    def test_head_or_generation_change_after_judgement_has_no_terminal_effect(self):
        for field, value in [('sha', 'd' * 40), ('gui_receipt', {'outcome': 'pass', 'generation': 'new'})]:
            api, data = FakeAPI(), ready()
            changed = dict(data, **{field: value})
            with patch.object(controller, 'snapshot', side_effect=[data, data, changed]), patch.object(controller, 'judge', return_value={'decision': 'ready', 'reason': 'passed old data'}):
                controller.process(api, 1, self.directory)
            self.assertEqual(len(api.writes), 1)  # claim only, never a terminal status
            self.assertEqual(api.merges, [])

    def test_last_moment_regression_prevents_merge(self):
        api, data = FakeAPI(), ready()
        regressed = dict(data, ci_ready=False)
        with patch.object(controller, 'snapshot', side_effect=[data, data, data, regressed]), patch.object(controller, 'judge', return_value={'decision': 'ready', 'reason': 'initial checks passed'}):
            controller.process(api, 1, self.directory)
        self.assertEqual(api.merges, [])

    def test_failed_judge_is_blocked_and_not_retried(self):
        api, data = FakeAPI(), ready()
        with patch.object(controller, 'snapshot', return_value=data), patch.object(controller, 'judge', side_effect=ValueError('credential or inference failure')) as judge:
            controller.process(api, 1, self.directory)
            controller.process(api, 1, self.directory)
        self.assertEqual(judge.call_count, 1)
        self.assertEqual(api.merges, [])
        self.assertEqual(api.writes[-1][2], 'error')

class FixEvidenceTests(unittest.TestCase):
    def test_final_fix_receipt_must_match_current_fingerprint(self):
        import final_fix_bridge as bridge
        data = ready()
        key = fingerprint(data)
        proof = {'schema_version': 1, 'run_id': '123', 'run_attempt': '1',
                 'evidence_key': key, 'snapshot': data,
                 'copilot': {'decision': 'fix', 'reason': 'save behavior failed'}, 'effect': 'fix requested'}
        api = FakeAPI()
        api.rows['hane/final-judge'] = {'id': 10, 'state': 'failure',
            'description': f'Final fix v1 {SHA[:12]} e{key}',
            'target_url': 'https://github.com/owner/repo/actions/runs/123'}
        run = {'status': 'completed', 'head_branch': 'main', 'path': '.github/workflows/final-judge.yml',
               'event': 'workflow_run', 'run_attempt': 1}
        with patch.object(bridge, 'snapshot', return_value=data), patch.object(api, 'api', return_value=run), patch.object(bridge, 'artifact_json', return_value=proof):
            self.assertEqual(bridge.evidence(api, 1, SHA), proof)
            proof['snapshot'] = dict(data, gui_receipt={'outcome': 'pass', 'generation': 'older'})
            with self.assertRaises(ValueError):
                bridge.evidence(api, 1, SHA)

    def test_pre_gui_route_does_not_depend_on_final_artifacts(self):
        import final_fix_bridge as bridge
        api = FakeAPI()
        with patch.object(bridge, 'snapshot', side_effect=AssertionError('must not read final evidence')):
            self.assertIsNone(bridge.evidence(api, 1, SHA))


class LiveProbeTests(unittest.TestCase):
    def test_probe_calls_judge_for_every_terminal_outcome_without_writes(self):
        import final_judge_probe as probe
        api, data = FakeAPI(), ready()
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, {
            'GITHUB_WORKFLOW_REF': 'owner/repo/.github/workflows/final-judge-probe.yml@refs/heads/main',
            'GITHUB_ACTOR': 'owner', 'INPUT_PR': '1', 'PROBE_RECEIPT': str(Path(tmp) / 'proof.json')}), \
                patch.object(probe, 'GitHub', return_value=api), \
                patch.object(probe, 'snapshot', return_value=data), \
                patch.object(probe, 'judge', return_value={'decision': 'blocked', 'reason': 'probe'}) as judge:
            probe.main()
            recorded = json.loads((Path(tmp) / 'proof.json').read_text())
        self.assertEqual(judge.call_count, 3)
        self.assertEqual([c['gui_outcome'] for c in recorded['cases']], ['pass', 'fail', 'blocked'])
        self.assertEqual([c['fault_injected'] for c in recorded['cases']], [False, True, True])
        self.assertEqual(api.writes, [])
        self.assertEqual(api.merges, [])


if __name__ == '__main__':
    unittest.main()
