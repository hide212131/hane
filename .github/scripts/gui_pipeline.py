"""Trusted orchestration only. The macOS worker receives no write token."""
import json
import os
from pathlib import Path
import sys
from datetime import datetime, timedelta, timezone
sys.path.insert(0, str(Path(__file__).resolve().parent))
from pipeline_api import GitHub
from gui_policy import CONTEXT, PROCEDURE, STATUS_VERSION, authenticated_receipt, gui_state, parse_time, receipt, validate_receipt
from gui_artifacts import artifact_json


def now():
    return datetime.now(timezone.utc)


def output(key, value):
    with open(os.environ['GITHUB_OUTPUT'], 'a') as file:
        file.write(f'{key}={value}\n')


def request_for(pr):
    run_id, attempt = os.environ['GITHUB_RUN_ID'], os.environ['GITHUB_RUN_ATTEMPT']
    generation = f'{run_id}-{attempt}'
    stamp = now()
    return {'pr_number': pr['number'], 'sha': pr['head']['sha'], 'repository': os.environ['GITHUB_REPOSITORY'],
            'control_sha': os.environ['CONTROL_SHA'], 'procedure_version': PROCEDURE, 'generation': generation,
            'request_id': f'gui-{generation}-pr{pr["number"]}', 'run_id': run_id, 'run_attempt': attempt,
            'created_at': stamp.isoformat(), 'expires_at': (stamp + timedelta(hours=1)).isoformat()}


def publish(api, request, outcome):
    # The serialized resolver owns claims; report owns only its exact generation.
    state = {'pending': 'pending', 'pass': 'success', 'fail': 'failure', 'blocked': 'error'}[outcome]
    api.post_status(request['sha'], CONTEXT, state,
                    f'GUI {outcome} {STATUS_VERSION} {request["sha"][:12]} g{request["generation"]}')


def current(api, request, require_claim=True):
    pr = api.pr(request['pr_number'])
    if not api.trusted(pr) or pr['head']['sha'] != request['sha']:
        return False
    if require_claim:
        return gui_state(api.statuses(request['sha']).get(CONTEXT, {}), request['sha']) == ('pending', request['generation'])
    return True


def terminal_receipt_retained(api, pr, state):
    outcome, generation = state
    number = pr['number']
    run_id, attempt = generation.split('-')
    run = api.api(api.repo(f'actions/runs/{run_id}/attempts/{attempt}'))
    if run.get('status') != 'completed':
        # The report publishes its status before uploading the receipt. Give
        # that exact attempt time to finish before deciding evidence is lost.
        return True
    try:
        try:
            proof = artifact_json(api, run_id, f'gui-receipt-{number}-{generation}', 'gui-receipt.json')
        except (ValueError, KeyError):
            if outcome != 'blocked':
                return False
            proof = artifact_json(api, run_id, f'gui-recovered-{generation}', f'{number}.json')
        authenticated_receipt(proof, pr['head']['sha'], number, api.repository, state, run)
        return True
    except (ValueError, KeyError):
        return False


def eligible(api, request):
    data = api.evidence(api.pr(request['pr_number']))
    return all(data[k] for k in ('gui_required', 'classified', 'review_ready', 'ci_ready'))


def retire(api, request):
    # Outside the terminal receipt grammar: this is invalidated work, not a
    # failed GUI observation. A later eligible resolver can claim a fresh run.
    if current(api, request):
        api.post_status(request['sha'], CONTEXT, 'error',
                        f'GUI superseded {STATUS_VERSION} {request["sha"][:12]} g{request["generation"]}')


def resolve(api):
    requested = os.environ.get('INPUT_PR', '').strip()
    force = os.environ.get('INPUT_FORCE', 'false') == 'true'
    if force and os.environ.get('GITHUB_ACTOR') != api.repository.split('/')[0]:
        raise ValueError('only the owner can request a fresh terminal-result generation')
    numbers = [int(requested)] if requested else [p['number'] for p in api.pages(api.repo('pulls?state=open'))]
    matrix = []
    recovered = Path(os.environ['RUNNER_TEMP']) / 'gui-recovered'
    recovered.mkdir(exist_ok=True)
    for number in numbers:
        try:
            pr = api.pr(number)
            if not api.trusted(pr):
                continue
            wanted_sha = os.environ.get('INPUT_SHA', '').strip()
            if requested and wanted_sha and wanted_sha != pr['head']['sha']:
                continue
            old_status = api.statuses(pr['head']['sha']).get(CONTEXT, {})
            old = gui_state(old_status, pr['head']['sha'])
            request = request_for(pr)
            if not eligible(api, request):
                if old and old[0] == 'pending':
                    retire(api, dict(request, generation=old[1]))
                continue
            lost_generation = None
            if old and old[0] != 'pending' and not force:
                if terminal_receipt_retained(api, pr, old):
                    continue
                lost_generation, old = old[1], None
            request = request_for(pr)
            if lost_generation:
                request.update(replaces_generation=lost_generation, replacement_reason='receipt missing or expired')
            if old and old[0] == 'pending':
                old_id, old_attempt = old[1].split('-')
                old_run = api.api(api.repo(f'actions/runs/{old_id}/attempts/{old_attempt}'))
                age = (now() - parse_time(old_status['created_at'])).total_seconds()
                if old_run.get('status') != 'completed' and age < 3600:
                    continue
                # Recover a lost report as a new blocked generation. Keep its
                # receipt in THIS run, never attribute new evidence to old runs.
                proof = receipt(request, 'blocked', f'Previous GUI run {old_run["id"]} ended or expired without a terminal report')
                if current(api, request, require_claim=False):
                    (recovered / f'{number}.json').write_text(json.dumps(proof, ensure_ascii=False, indent=2))
                    publish(api, request, 'blocked')
                continue
            data = api.evidence(pr)
            if not (data['gui_required'] and data['classified'] and data['review_ready'] and data['ci_ready']):
                continue
            if current(api, request, require_claim=False):
                publish(api, request, 'pending')
                matrix.append(request)
        except Exception as exc:
            # A read/eligibility error cannot authorize target execution.
            print(f'PR {number}: no GUI execution: {exc}', file=sys.stderr)
    output('matrix', json.dumps({'include': matrix}, separators=(',', ':')))
    output('has_work', 'true' if matrix else 'false')


def begin(api, request):
    allowed = (now() < parse_time(request['expires_at']) and current(api, request)
               and request['repository'] == api.repository)
    if allowed and not eligible(api, request):
        # Worker has read-only credentials. Serialized report/resolve retires
        # the claim; begin must never acquire status-write permissions.
        allowed = False
    output('proceed', 'true' if allowed else 'false')


def report(api, request):
    if not current(api, request):
        print('Stale GUI generation: no status written')
        return
    if not eligible(api, request):
        retire(api, request)
        return
    evidence_dir = Path(os.environ['EVIDENCE_DIR'])
    raw = None
    try:
        result_path = evidence_dir / 'result.json'
        if result_path.stat().st_size > 1024 * 1024:
            raise ValueError('GUI result exceeds size bound')
        raw = json.loads(result_path.read_text())
        jobs = api.pages(api.repo(f'actions/runs/{request["run_id"]}/attempts/{request["run_attempt"]}/jobs'), 'jobs')
        worker = next((j for j in jobs if j['name'] == f'GUI worker #{request["pr_number"]}'), {})
        outcome = validate_receipt(raw, request, evidence_dir, worker.get('conclusion'))
        reason = raw.get('overall_reason', '')
    except Exception as exc:
        outcome, reason = 'blocked', f'GUI receipt rejected or worker incomplete: {exc}'
    proof = receipt(request, outcome, reason, raw, evidence_dir)
    Path(os.environ['RECEIPT_PATH']).write_text(json.dumps(proof, ensure_ascii=False, indent=2))
    # Recheck after reading artifacts and worker metadata, immediately before POST.
    if current(api, request):
        if not eligible(api, request):
            retire(api, request)
            return
        publish(api, request, outcome)
        print(f'GUI {outcome} for PR {request["pr_number"]} at {request["sha"]}')


def main():
    api = GitHub()
    mode = sys.argv[1]
    if mode == 'resolve':
        resolve(api)
    else:
        request = json.loads(os.environ['REQUEST_JSON'])
        if mode == 'begin':
            begin(api, request)
        elif mode == 'report':
            report(api, request)
        else:
            raise ValueError('unknown controller mode')


if __name__ == '__main__':
    main()
