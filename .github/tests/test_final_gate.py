"""Final-judge boundaries: terminal GUI outcomes, stale data and actual merge CAS."""
from copy import deepcopy
from pathlib import Path
import json
import os
import sys
import tempfile
import unittest
from unittest.mock import MagicMock, patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import final_pipeline as controller
from final_policy import authenticated_receipt, fingerprint, final_state, gate, may_judge, parse_decision
from claude_fix_state import routing_allows_fix
from pipeline_api import CI_NAMES
from gui_policy import PROCEDURE

SHA = 'a' * 40


def ready(gui=True):
    return {'pr_number': 1, 'sha': SHA, 'repository': 'owner/repo', 'trusted': True, 'mergeable': True,
            'ci_ready': True, 'review_ready': True, 'classified': True, 'gui_required': gui,
            'unresolved_threads': [], 'blocking_reviews': [], 'required_checks': sorted(CI_NAMES),
            'workflow_changes': False, 'auto_merge': True,
            'gui_receipt': {'outcome': 'pass'} if gui else None}


class PolicyTests(unittest.TestCase):
    def test_no_gui_snapshot_ignores_expired_old_gui_receipt(self):
        api = MagicMock(repository='owner/repo')
        api.pr.return_value = {'title': 'Docs', 'mergeable': True, 'labels': []}
        api.trusted.return_value = True
        api.evidence.return_value = dict(ready(gui=False), files=[], statuses={
            controller.GUI_CONTEXT: {'id': 5, 'state': 'success', 'description': 'old receipt'}})
        api.pages.return_value = []
        api.api.return_value = []
        with patch.object(controller, 'gui_receipt', side_effect=ValueError('expired')) as read, \
                patch.object(controller, 'review_threads', return_value=[]):
            data = controller.snapshot(api, 1)
        read.assert_not_called()
        self.assertIsNone(data['gui_receipt'])
        self.assertNotIn(controller.GUI_CONTEXT, data['statuses'])

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
                   'generation': '123-2', 'run_id': '123', 'run_attempt': '2', 'request_id': 'gui-123-2-pr1',
                   'procedure_version': PROCEDURE}
        proof = {'schema_version': 1, 'policy_version': 'v1', 'request': request, 'outcome': 'pass'}
        run = {'id': 123, 'run_attempt': 2, 'status': 'completed', 'path': '.github/workflows/gui-validation.yml',
               'head_branch': 'main', 'head_sha': 'b' * 40, 'event': 'workflow_dispatch'}
        self.assertEqual(authenticated_receipt(proof, SHA, 1, 'owner/repo', ('pass', '123-2'), run), proof)
        for field, value in [('run_attempt', 3), ('head_branch', 'untrusted'), ('path', '.github/workflows/ci.yml'),
                             ('head_sha', 'c' * 40), ('status', 'in_progress'), ('event', 'push')]:
            bad = dict(run, **{field: value})
            with self.subTest(field=field), self.assertRaises(ValueError):
                authenticated_receipt(proof, SHA, 1, 'owner/repo', ('pass', '123-2'), bad)
        request['procedure_version'] = 'hosted-gui-interaction/2'
        with self.assertRaises(ValueError):
            authenticated_receipt(proof, SHA, 1, 'owner/repo', ('pass', '123-2'), run)

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

    def post_status(self, sha, context, state, description, run_id=None):
        self.writes.append((sha, context, state, description))
        self.rows[context] = {'state': state, 'description': description,
                              'target_url': f'https://github.com/{self.repository}/actions/runs/{run_id}'}

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

    def test_refused_or_uncertain_merge_is_blocked_without_repeating_judge(self):
        for response in ({'merged': False, 'message': 'branch protection refused'}, ValueError('network failure')):
            api, data = FakeAPI(), ready()
            options = {'side_effect': response} if isinstance(response, Exception) else {'return_value': response}
            with self.subTest(response=response), patch.object(controller, 'snapshot', return_value=data), \
                    patch.object(controller, 'judge', return_value={'decision': 'ready', 'reason': 'passed'}) as judge, \
                    patch.object(api, 'api', **options) as merge:
                controller.process(api, 1, self.directory)
                controller.process(api, 1, self.directory)
            self.assertEqual(judge.call_count, 1)
            self.assertEqual(merge.call_count, 1)
            self.assertEqual(api.writes[-1][2], 'error')
            proof = json.loads((self.directory / '1.json').read_text())
            self.assertEqual(proof['effect'], 'blocked')
            self.assertTrue(proof['merge_error'])
            self.assertNotIn('merge_sha', proof)

    def test_head_or_generation_change_after_judgement_has_no_terminal_effect(self):
        for field, value in [('sha', 'd' * 40), ('gui_receipt', {'outcome': 'pass', 'generation': 'new'})]:
            api, data = FakeAPI(), ready()
            changed = dict(data, **{field: value})
            with patch.object(controller, 'snapshot', side_effect=[data, data, changed]), patch.object(controller, 'judge', return_value={'decision': 'ready', 'reason': 'passed old data'}):
                controller.process(api, 1, self.directory)
            self.assertEqual(len(api.writes), 1)  # claim only, never a terminal status
            self.assertEqual(api.merges, [])

    def test_unavailable_merge_response_is_reconciled_against_exact_head(self):
        for actual_head in (SHA, 'd' * 40):
            api, data = FakeAPI(), ready()
            confirmed = {'merged': True, 'head': {'sha': actual_head}, 'merge_commit_sha': 'c' * 40}
            with self.subTest(head=actual_head), patch.object(controller, 'snapshot', return_value=data), \
                    patch.object(controller, 'judge', return_value={'decision': 'ready', 'reason': 'passed'}), \
                    patch.object(api, 'api', side_effect=TimeoutError('response lost')), \
                    patch.object(api, 'pr', create=True, return_value=confirmed) as reread:
                controller.process(api, 1, self.directory)
            reread.assert_called_once_with(1)
            proof = json.loads((self.directory / '1.json').read_text())
            self.assertEqual(proof['effect'], 'merged' if actual_head == SHA else 'blocked')
            if actual_head == SHA:
                self.assertEqual(proof['merge_sha'], 'c' * 40)
                self.assertTrue(proof['merge_reconciled_after_lost_response'])
            else:
                self.assertNotIn('merge_sha', proof)

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

    def test_lost_fix_receipt_gets_one_fresh_judgement_then_explicit_block(self):
        data = ready()
        key = fingerprint(data)
        run = {'status': 'completed', 'run_attempt': 1, 'head_branch': 'main',
               'path': '.github/workflows/final-judge.yml', 'event': 'workflow_run'}
        for count in (1, 2):
            api = FakeAPI()
            api.rows[controller.CONTEXT] = {'state': 'failure',
                'description': f'Final fix v1 {SHA[:12]} e{key}',
                'target_url': 'https://github.com/owner/repo/actions/runs/100/attempts/1'}
            history = [{'context': controller.CONTEXT, 'state': 'pending',
                'description': f'Final pending v1 {SHA[:12]} e{key}', 'target_url': f'run/{i}'} for i in range(count)]
            with self.subTest(count=count), patch.object(controller, 'snapshot', return_value=data), \
                    patch.object(api, 'api', return_value=run), \
                    patch.object(api, 'pages', create=True, return_value=history), \
                    patch.object(controller, 'artifact_json', side_effect=ValueError('expired')), \
                    patch.object(controller, 'judge', return_value={'decision': 'fix', 'reason': 'save fails'}) as judge:
                controller.process(api, 1, self.directory)
            self.assertEqual(judge.call_count, int(count == 1))
            proof = json.loads((self.directory / '1.json').read_text())
            self.assertEqual(proof['effect'], 'fix requested' if count == 1 else 'blocked')
            self.assertEqual(api.writes[-1][2], 'failure' if count == 1 else 'error')

    def test_retained_fix_receipt_skips_paid_replay(self):
        data, api = ready(), FakeAPI()
        key = fingerprint(data)
        api.rows[controller.CONTEXT] = {'state': 'failure',
            'description': f'Final fix v1 {SHA[:12]} e{key}',
            'target_url': 'https://github.com/owner/repo/actions/runs/100/attempts/1'}
        proof = {'schema_version': 1, 'run_id': '100', 'run_attempt': '1', 'snapshot': data,
                 'evidence_key': key, 'effect': 'fix requested', 'copilot': {'decision': 'fix'}}
        run = {'status': 'completed', 'run_attempt': 1, 'head_branch': 'main',
               'path': '.github/workflows/final-judge.yml', 'event': 'workflow_run'}
        with patch.object(controller, 'snapshot', return_value=data), patch.object(api, 'api', return_value=run), \
                patch.object(controller, 'artifact_json', return_value=proof), patch.object(controller, 'judge') as judge:
            controller.process(api, 1, self.directory)
        judge.assert_not_called()
        self.assertEqual(api.writes, [])

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
            'target_url': 'https://github.com/owner/repo/actions/runs/123/attempts/1'}
        run = {'status': 'completed', 'head_branch': 'main', 'path': '.github/workflows/final-judge.yml',
               'event': 'workflow_run', 'run_attempt': 1}
        with patch.object(bridge, 'snapshot', return_value=data), patch.object(api, 'api', return_value=run), patch.object(bridge, 'artifact_json', return_value=proof):
            self.assertEqual(bridge.evidence(api, 1, SHA), proof)
            proof['snapshot'] = dict(data, gui_receipt={'outcome': 'pass', 'generation': 'older'})
            with self.assertRaises(ValueError):
                bridge.evidence(api, 1, SHA)

    def test_worker_cannot_adopt_a_new_final_generation_mid_implementation(self):
        from final_fix_bridge import same_authorization
        original = {'run_id': '123', 'run_attempt': '1', 'evidence_key': 'key', 'snapshot': ready()}
        self.assertTrue(same_authorization(original, deepcopy(original)))
        for field, value in [('run_id', '456'), ('run_attempt', '2'), ('evidence_key', 'other'),
                             ('snapshot', dict(ready(), gui_receipt={'outcome': 'fail', 'generation': 'new'}))]:
            self.assertFalse(same_authorization(original, dict(original, **{field: value})))
        self.assertFalse(same_authorization(original, None))
        self.assertFalse(same_authorization(None, original))
        self.assertTrue(same_authorization(None, None))

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
