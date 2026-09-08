"""Authenticate final fix evidence before dispatch and immediately before Claude."""
import json
import os
from pathlib import Path
import re
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
from final_pipeline import artifact_json, snapshot
from final_policy import CONTEXT, final_state, fingerprint
from pipeline_api import GitHub


def evidence(api, number, sha):
    statuses = api.statuses(sha)
    final = statuses.get(CONTEXT, {})
    pre = statuses.get('hane/copilot-routing', {})
    if final.get('state') != 'failure' or final.get('id', 0) < pre.get('id', 0):
        return None
    data = snapshot(api, number)
    if data['sha'] != sha:
        raise ValueError('final fix head changed')
    status = api.statuses(sha).get(CONTEXT, {})
    key = fingerprint(data)
    if final_state(status, sha, key) != 'fix':
        return None
    match = re.fullmatch(r'https://github.com/' + re.escape(api.repository) + r'/actions/runs/(\d+)/attempts/(\d+)', status.get('target_url', ''))
    if not match:
        raise ValueError('invalid final fix run URL')
    run_id, attempt = match[1], match[2]
    run = api.api(api.repo(f'actions/runs/{run_id}/attempts/{attempt}'))
    if (str(run.get('run_attempt')) != attempt or run.get('status') != 'completed' or run.get('head_branch') != 'main'
            or run.get('path') != '.github/workflows/final-judge.yml'
            or run.get('event') not in ('workflow_dispatch', 'workflow_run', 'schedule')):
        raise ValueError('untrusted or incomplete final judge run')
    proof = artifact_json(api, run_id, f'final-receipts-{run_id}-{attempt}', f'{number}.json')
    if (proof.get('schema_version') != 1 or proof.get('run_id') != run_id
            or proof.get('run_attempt') != attempt or proof.get('evidence_key') != key
            or proof.get('snapshot') != data or proof.get('copilot', {}).get('decision') != 'fix'
            or proof.get('effect') != 'fix requested' or data['workflow_changes']):
        raise ValueError('final fix receipt does not match current evidence')
    return proof


def main():
    api = GitHub()
    if sys.argv[1] == 'evidence':
        number, sha = int(os.environ['PR_NUMBER']), os.environ['TARGET_SHA']
        proof = evidence(api, number, sha)
        path = Path(os.environ['FINAL_EVIDENCE_PATH'])
        if '--verify' in sys.argv[2:]:
            original = json.loads(path.read_text())
            if not same_authorization(original, proof):
                raise ValueError('final fix authorization changed since implementation evidence was captured')
        else:
            path.write_text(json.dumps(proof, ensure_ascii=False))
        # If a final fix is the latest authorization, a stale/missing matching
        # receipt must stop the paid invocation, not fall back to old findings.
        latest = api.statuses(sha)
        final = latest.get(CONTEXT, {})
        pre = latest.get('hane/copilot-routing', {})
        if final.get('id', 0) > pre.get('id', 0) and final.get('state') == 'failure' and proof is None:
            raise ValueError('latest final fix authorization is stale')
        return
    requested = os.environ.get('INPUT_PR', '').strip()
    numbers = [int(requested)] if requested else [p['number'] for p in api.pages(api.repo('pulls?state=open'))]
    for number in numbers:
        try:
            pr = api.pr(number)
            if not api.trusted(pr):
                continue
            proof = evidence(api, number, pr['head']['sha'])
            if not proof:
                continue
            # Existing worker owns paid-attempt limits, pending leases and
            # duplicate dispatch rejection. The bridge grants no manual retry.
            api.api(api.repo('dispatches'), {'event_type': 'hane-claude-fix', 'client_payload': {
                'pr_number': number, 'target_sha': pr['head']['sha'], 'manual_retry': False}})
        except Exception as exc:
            print(f'PR {number}: no final fix dispatch: {exc}', file=sys.stderr)


def same_authorization(original, current):
    if original is None and current is None:
        return True
    return (isinstance(original, dict) and isinstance(current, dict)
            and all(original.get(key) == current.get(key) and original.get(key) is not None
                    for key in ('run_id', 'run_attempt', 'evidence_key'))
            and original.get('snapshot') == current.get('snapshot'))


if __name__ == '__main__':
    main()
