"""Owner-run live Copilot boundary probe; never publishes statuses or merges.

Uses one real current GUI-pass snapshot. Failure/blocked copies are explicitly
fault-injected test inputs, not claims that the application failed.
"""
from copy import deepcopy
import json
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
from final_pipeline import judge, snapshot
from final_policy import gate, may_judge
from pipeline_api import GitHub


def main():
    api = GitHub()
    if (not os.environ['GITHUB_WORKFLOW_REF'].endswith('@refs/heads/main')
            or os.environ['GITHUB_ACTOR'] != api.repository.split('/')[0]):
        raise ValueError('probe requires owner dispatch from main')
    data = snapshot(api, int(os.environ['INPUT_PR']))
    if gate(data) or not data['gui_required']:
        raise ValueError('probe requires complete current real GUI-pass evidence')
    records = []
    for outcome in ('pass', 'fail', 'blocked'):
        case = deepcopy(data)
        case['validation_probe'] = {'no_external_effects': True, 'fault_injected': outcome != 'pass',
                                    'original_gui_outcome': 'pass', 'tested_terminal_outcome': outcome}
        proof = case['gui_receipt']
        proof['outcome'] = outcome
        if outcome != 'pass':
            proof['reason'] = 'FAULT-INJECTED VALIDATION PROBE: ' + ('save-content assertion failed' if outcome == 'fail' else 'worker interrupted before input')
            raw = proof.get('result')
            if raw:
                raw['overall_result'] = outcome
                raw['overall_reason'] = proof['reason']
                raw['scenarios'][0]['result'] = outcome
                raw['scenarios'][0]['steps'] = [{'name': 'probe_fault', 'result': outcome, 'reason': proof['reason']}]
        if not may_judge(case):
            raise AssertionError('terminal GUI outcome did not reach judge')
        decision = judge(case)
        denials = gate(case)
        if outcome != 'pass' and not denials:
            raise AssertionError('nonpass GUI escaped deterministic gate')
        records.append({'gui_outcome': outcome, 'fault_injected': outcome != 'pass',
                        'copilot': decision, 'gate_denials': denials, 'external_effects': []})
        Path(os.environ['PROBE_RECEIPT']).write_text(json.dumps({'schema_version': 1,
            'source_sha': data['sha'], 'source_pr': data['pr_number'], 'cases': records}, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
