"""Pure GUI request/receipt policy. No network, tokens, or target execution."""
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re

POLICY = 'v1'
PROCEDURE = 'hosted-gui-interaction/5'
STATUS_VERSION = f'{POLICY}-p{PROCEDURE.rsplit("/", 1)[1]}'
CONTEXT = 'hane/gui-validation'
STATUS = re.compile(r'GUI (pending|pass|fail|blocked) ' + re.escape(STATUS_VERSION) + r' ([0-9a-f]{12}) g([0-9]+-[0-9]+)')
INLINE_FIXTURE_ORIGINAL = (
    '# hosted gui interaction inline syntax spike\n'
    '\n'
    '**bold *italic* combo** boundary line.\n'
    '\n'
    'this inline `code\n'
    'span` crosses a line.\n'
    '\n'
    '> quote with **bold** inside.\n'
    '\n'
    '- list item with *italic* inside.\n'
)
REQUIRED_STEPS = {
    'ascii_edit_save_undo_redo_reopen': {'launch', 'window_discovery', 'edit_save', 'append_save', 'undo_save', 'redo_save', 'capture_before', 'capture_after', 'capture_reopen', 'visible_saved_text', 'reopen_content_check', 'cleanup'},
    'japanese_ime_input': {'query_current_source', 'list_input_sources', 'select_japanese_source', 'launch', 'window_discovery', 'ime_input_save', 'capture_before', 'capture_after', 'cleanup', 'restore_input_source'},
    'os_scroll': {'launch', 'window_discovery', 'capture_before', 'os_wheel', 'capture_after', 'visible_scroll', 'scroll_preserves_document', 'cleanup'},
    'inline_syntax_boundary': {
        'launch', 'window_discovery', 'capture_before',
        'boundary_click_edit_bold_italic', 'boundary_click_edit_code_span',
        'boundary_click_edit_quote', 'boundary_click_edit_list',
        'capture_boundary_click_edit_bold_italic_0', 'boundary_click_edit_bold_italic_check_0',
        'capture_boundary_click_edit_bold_italic_1', 'boundary_click_edit_bold_italic_check_1',
        'capture_boundary_click_edit_code_span_0', 'boundary_click_edit_code_span_check_0',
        'capture_boundary_click_edit_code_span_1', 'boundary_click_edit_code_span_check_1',
        'capture_boundary_click_edit_quote_0', 'boundary_click_edit_quote_check_0',
        'capture_boundary_click_edit_list_0', 'boundary_click_edit_list_check_0',
        'boundary_caret_navigation',
        'capture_boundary_caret_navigation_0', 'boundary_caret_navigation_check_0',
        'capture_boundary_caret_navigation_1', 'boundary_caret_navigation_check_1',
        'capture_boundary_caret_navigation_2', 'boundary_caret_navigation_check_2',
        'boundary_ime_input', 'capture_boundary_ime_input', 'boundary_ime_input_check',
        'restore_boundary_ime_input_source',
        'drag_select_delete_undo_redo', 'capture_drag_select_state0',
        'drag_select_delete_undo_redo_check',
        'delimiter_toggle_star', 'capture_delimiter_toggle_star_initial_closed',
        'capture_delimiter_toggle_star_unclosed', 'capture_delimiter_toggle_star_closed',
        'delimiter_toggle_star_check',
        'delimiter_toggle_bold', 'capture_delimiter_toggle_bold_initial_closed',
        'capture_delimiter_toggle_bold_unclosed', 'capture_delimiter_toggle_bold_closed',
        'delimiter_toggle_bold_check',
        'delimiter_toggle_code', 'capture_delimiter_toggle_code_initial_closed',
        'capture_delimiter_toggle_code_unclosed', 'capture_delimiter_toggle_code_closed',
        'delimiter_toggle_code_check',
        'multiline_code_span_close_toggle', 'capture_multiline_code_span_close_toggle_initial_closed',
        'capture_multiline_code_span_close_toggle_unclosed', 'capture_multiline_code_span_close_toggle_closed',
        'multiline_code_span_close_toggle_check',
        'capture_after', 'cleanup',
        'launch_reopen', 'window_discovery_reopen', 'capture_reopen',
        'visible_saved_text', 'reopen_content_check', 'cleanup_reopen',
    },
}
REQUIRED_IMAGES = [
    'ascii_edit_save_undo_redo_reopen/before.png',
    'ascii_edit_save_undo_redo_reopen/after.png',
    'ascii_edit_save_undo_redo_reopen/reopen/reopen.png',
    'japanese_ime_input/before.png', 'japanese_ime_input/after.png',
    'os_scroll/before.png', 'os_scroll/after.png',
    'inline_syntax_boundary/before.png', 'inline_syntax_boundary/after.png',
    'inline_syntax_boundary/reopen/reopen.png',
    'inline_syntax_boundary/boundary_click_edit_bold_italic_0.png',
    'inline_syntax_boundary/boundary_click_edit_bold_italic_1.png',
    'inline_syntax_boundary/boundary_click_edit_code_span_0.png',
    'inline_syntax_boundary/boundary_click_edit_code_span_1.png',
    'inline_syntax_boundary/boundary_click_edit_quote_0.png',
    'inline_syntax_boundary/boundary_click_edit_list_0.png',
    'inline_syntax_boundary/boundary_caret_navigation_0.png',
    'inline_syntax_boundary/boundary_caret_navigation_1.png',
    'inline_syntax_boundary/boundary_caret_navigation_2.png',
    'inline_syntax_boundary/boundary_ime_input.png',
    'inline_syntax_boundary/drag_select_state0.png',
    'inline_syntax_boundary/delimiter_toggle_star_initial_closed.png',
    'inline_syntax_boundary/delimiter_toggle_star_initial_closed.body.png',
    'inline_syntax_boundary/delimiter_toggle_star_unclosed.png',
    'inline_syntax_boundary/delimiter_toggle_star_unclosed.body.png',
    'inline_syntax_boundary/delimiter_toggle_star_closed.png',
    'inline_syntax_boundary/delimiter_toggle_star_closed.body.png',
    'inline_syntax_boundary/delimiter_toggle_bold_initial_closed.png',
    'inline_syntax_boundary/delimiter_toggle_bold_initial_closed.body.png',
    'inline_syntax_boundary/delimiter_toggle_bold_unclosed.png',
    'inline_syntax_boundary/delimiter_toggle_bold_unclosed.body.png',
    'inline_syntax_boundary/delimiter_toggle_bold_closed.png',
    'inline_syntax_boundary/delimiter_toggle_bold_closed.body.png',
    'inline_syntax_boundary/delimiter_toggle_code_initial_closed.png',
    'inline_syntax_boundary/delimiter_toggle_code_initial_closed.body.png',
    'inline_syntax_boundary/delimiter_toggle_code_unclosed.png',
    'inline_syntax_boundary/delimiter_toggle_code_unclosed.body.png',
    'inline_syntax_boundary/delimiter_toggle_code_closed.png',
    'inline_syntax_boundary/delimiter_toggle_code_closed.body.png',
    'inline_syntax_boundary/multiline_code_span_close_toggle_initial_closed.png',
    'inline_syntax_boundary/multiline_code_span_close_toggle_initial_closed.body.png',
    'inline_syntax_boundary/multiline_code_span_close_toggle_unclosed.png',
    'inline_syntax_boundary/multiline_code_span_close_toggle_unclosed.body.png',
    'inline_syntax_boundary/multiline_code_span_close_toggle_closed.png',
    'inline_syntax_boundary/multiline_code_span_close_toggle_closed.body.png',
]


def latest(statuses):
    result = {}
    for status in statuses:
        context = status.get('context')
        if isinstance(status.get('id'), int) and status['id'] > result.get(context, {}).get('id', -1):
            result[context] = status
    return result


def gui_state(status, sha):
    match = STATUS.fullmatch(status.get('description', ''))
    if not match or match[2] != sha[:12]:
        return None
    outcome = match[1]
    expected = {'pending': 'pending', 'pass': 'success', 'fail': 'failure', 'blocked': 'error'}[outcome]
    return (outcome, match[3]) if status.get('state') == expected else None


def required_from_files(files, expected_count, force=False):
    if not isinstance(files, list) or len(files) != expected_count:
        raise ValueError('incomplete changed-file evidence')
    if force or not files:
        return True
    for item in files:
        if not isinstance(item, dict) or not isinstance(item.get('filename'), str) or not item['filename']:
            raise ValueError('invalid filename')
        names = [item['filename']]
        if 'previous_filename' in item:
            names.append(item['previous_filename'])
        for name in names:
            if not isinstance(name, str) or not name or '..' in name.split('/'):
                raise ValueError('invalid filename')
            if not (name.startswith(('docs/', '.github/')) or ('/' not in name and name.endswith('.md'))):
                return True
    return False


def classification_matches(status, sha, required):
    word = 'required' if required else 'not required'
    return (status.get('description') == f'GUI validation {word} (v1) for {sha[:12]}'
            and status.get('state') == ('failure' if required else 'success'))


def review_ready(statuses, sha):
    review = statuses.get('hane/codex-review', {})
    if review.get('state') == 'success' and review.get('description') == f'Codex review clean for {sha[:12]}':
        return True
    routing = statuses.get('hane/copilot-routing', {})
    return (review.get('state') == 'failure' and review.get('description') == f'Codex findings for {sha[:12]}'
            and routing.get('state') == 'success'
            and routing.get('description') == f'Copilot routing: continue-validation for {sha[:12]}'
            and routing.get('id', 0) > review.get('id', 0))


def parse_time(value):
    return datetime.fromisoformat(value.replace('Z', '+00:00')).astimezone(timezone.utc)


def _require_text(step, *names):
    for name in names:
        if not isinstance(step.get(name), str) or not step[name]:
            raise ValueError(f'missing inline evidence field: {name}')


def _require_image_ref(step, field, relative):
    _require_text(step, field)
    actual = Path(step[field])
    expected = Path(relative)
    if len(actual.parts) < len(expected.parts) or actual.parts[-len(expected.parts):] != expected.parts:
        raise ValueError(f'inline screenshot reference mismatch: {field}')


def _artifact_sha256(evidence_dir, relative):
    path = Path(evidence_dir) / relative
    if path.is_symlink() or not path.is_file() or path.stat().st_size < 100:
        raise ValueError('required screenshot missing')
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _delimiter_states(delimiter):
    closed = INLINE_FIXTURE_ORIGINAL + f' {delimiter}loose{delimiter} tail'
    unclosed = INLINE_FIXTURE_ORIGINAL + f' {delimiter}loose tail'
    return unclosed, closed


def _validate_inline_evidence(steps, evidence_dir):
    names = [step.get('name') for step in steps]
    if len(names) != len(set(names)):
        raise ValueError('duplicate inline evidence step')
    by_name = {step.get('name'): step for step in steps}

    boundary_images = {
        'boundary_click_edit_bold_italic_check_0': 'inline_syntax_boundary/boundary_click_edit_bold_italic_0.png',
        'boundary_click_edit_bold_italic_check_1': 'inline_syntax_boundary/boundary_click_edit_bold_italic_1.png',
        'boundary_click_edit_code_span_check_0': 'inline_syntax_boundary/boundary_click_edit_code_span_0.png',
        'boundary_click_edit_code_span_check_1': 'inline_syntax_boundary/boundary_click_edit_code_span_1.png',
        'boundary_click_edit_quote_check_0': 'inline_syntax_boundary/boundary_click_edit_quote_0.png',
        'boundary_click_edit_list_check_0': 'inline_syntax_boundary/boundary_click_edit_list_0.png',
        'boundary_ime_input_check': 'inline_syntax_boundary/boundary_ime_input.png',
    }
    boundary_expected = {
        'boundary_click_edit_bold_italic_check_0': INLINE_FIXTURE_ORIGINAL.replace('**bold', '**Zbold', 1),
        'boundary_click_edit_bold_italic_check_1': INLINE_FIXTURE_ORIGINAL.replace('combo**', 'comboZ**', 1),
        'boundary_click_edit_code_span_check_0': INLINE_FIXTURE_ORIGINAL.replace('`code', '`Zcode', 1),
        'boundary_click_edit_code_span_check_1': INLINE_FIXTURE_ORIGINAL.replace('span`', 'spanZ`', 1),
        'boundary_click_edit_quote_check_0': INLINE_FIXTURE_ORIGINAL.replace('quote with **bold', 'quote with **Zbold', 1),
        'boundary_click_edit_list_check_0': INLINE_FIXTURE_ORIGINAL.replace('item with *italic', 'item with *Zitalic', 1),
        'boundary_ime_input_check': INLINE_FIXTURE_ORIGINAL.replace('**bold', '**日本語bold', 1),
    }
    for name, image in boundary_images.items():
        step = by_name[name]
        _require_text(step, 'expected_after_insert', 'actual_after_insert',
                      'expected_after_undo', 'actual_after_undo')
        _require_image_ref(step, 'screenshot', image)
        if (step['expected_after_insert'] != boundary_expected[name]
                or step['expected_after_insert'] != step['actual_after_insert']):
            raise ValueError('inline insertion evidence mismatch')
        if (step['expected_after_undo'] != INLINE_FIXTURE_ORIGINAL
                or step['expected_after_undo'] != step['actual_after_undo']):
            raise ValueError('inline undo evidence mismatch')

    navigation = {
        'boundary_caret_navigation_check_0': (
            'inline_syntax_boundary/boundary_caret_navigation_0.png',
            'left_across_open_marker', 'left',
            INLINE_FIXTURE_ORIGINAL.replace('**bold', '*N*bold', 1),
        ),
        'boundary_caret_navigation_check_1': (
            'inline_syntax_boundary/boundary_caret_navigation_1.png',
            'right_into_visible_text', 'right',
            INLINE_FIXTURE_ORIGINAL.replace('**bold', '**bNold', 1),
        ),
        'boundary_caret_navigation_check_2': (
            'inline_syntax_boundary/boundary_caret_navigation_2.png',
            'up_from_multiline_code_close', 'up',
            INLINE_FIXTURE_ORIGINAL.replace('this inline `code', 'thisN inline `code', 1),
        ),
    }
    for name, (image, case, direction, expected) in navigation.items():
        step = by_name[name]
        _require_text(step, 'case', 'direction', 'expected_after_move_insert', 'actual_after_move_insert',
                      'expected_after_undo', 'actual_after_undo')
        _require_image_ref(step, 'screenshot', image)
        if step.get('count') != 1 or step['case'] != case or step['direction'] != direction:
            raise ValueError('caret-navigation operation evidence mismatch')
        if (step['expected_after_move_insert'] != expected
                or step['actual_after_move_insert'] != expected):
            raise ValueError('caret-navigation source-position mismatch')
        if (step['expected_after_undo'] != INLINE_FIXTURE_ORIGINAL
                or step['actual_after_undo'] != INLINE_FIXTURE_ORIGINAL):
            raise ValueError('caret-navigation undo evidence mismatch')

    drag = by_name['drag_select_delete_undo_redo_check']
    _require_text(drag, 'deleted_expected', 'deleted_actual',
                  'undo_actual', 'redo_actual', 'restored_actual')
    _require_image_ref(drag, 'screenshot', 'inline_syntax_boundary/drag_select_state0.png')
    deleted_expected = INLINE_FIXTURE_ORIGINAL.replace('old *italic* com', '', 1)
    if (drag['deleted_expected'] != deleted_expected
            or drag['deleted_actual'] != deleted_expected
            or drag['redo_actual'] != deleted_expected):
        raise ValueError('drag-selection evidence mismatch')
    if drag['undo_actual'] != INLINE_FIXTURE_ORIGINAL or drag['restored_actual'] != INLINE_FIXTURE_ORIGINAL:
        raise ValueError('drag-selection restore evidence mismatch')

    expected_delimiters = {'star': '*', 'bold': '**', 'code': '`'}
    for kind, delimiter in expected_delimiters.items():
        step = by_name[f'delimiter_toggle_{kind}_check']
        _require_text(step, 'delimiter', 'initial_pixel_digest', 'unclosed_pixel_digest', 'closed_pixel_digest',
                      'unclosed_expected', 'unclosed_actual', 'closed_expected', 'closed_actual')
        initial = f'inline_syntax_boundary/delimiter_toggle_{kind}_initial_closed.png'
        unclosed = f'inline_syntax_boundary/delimiter_toggle_{kind}_unclosed.png'
        closed = f'inline_syntax_boundary/delimiter_toggle_{kind}_closed.png'
        _require_image_ref(step, 'initial_screenshot', initial)
        _require_image_ref(step, 'unclosed_screenshot', unclosed)
        _require_image_ref(step, 'closed_screenshot', closed)
        if step['delimiter'] != delimiter:
            raise ValueError('delimiter kind/evidence mismatch')
        expected_unclosed, expected_closed = _delimiter_states(delimiter)
        if step['unclosed_expected'] != expected_unclosed or step['unclosed_actual'] != expected_unclosed:
            raise ValueError('delimiter-unclosed evidence mismatch')
        if step['closed_expected'] != expected_closed or step['closed_actual'] != expected_closed:
            raise ValueError('delimiter-closed evidence mismatch')
        body_images = (
            initial.removesuffix('.png') + '.body.png',
            unclosed.removesuffix('.png') + '.body.png',
            closed.removesuffix('.png') + '.body.png',
        )
        digests = (step['initial_pixel_digest'], step['unclosed_pixel_digest'], step['closed_pixel_digest'])
        if any(not re.fullmatch('[0-9a-f]{64}', digest) for digest in digests):
            raise ValueError('delimiter body-digest evidence invalid')
        artifact_digests = tuple(_artifact_sha256(evidence_dir, image) for image in body_images)
        if digests != artifact_digests:
            raise ValueError('delimiter body digest does not match artifact')
        if step.get('visual_transition_observed') is not True or step.get('closed_visual_restored') is not True:
            raise ValueError('delimiter visual transition evidence missing')
        if digests[0] != digests[2] or digests[1] == digests[2]:
            raise ValueError('delimiter visual transition evidence mismatch')

    span_step = by_name['multiline_code_span_close_toggle_check']
    _require_text(span_step, 'initial_pixel_digest', 'unclosed_pixel_digest', 'closed_pixel_digest',
                  'unclosed_expected', 'unclosed_actual', 'closed_expected', 'closed_actual')
    span_initial = 'inline_syntax_boundary/multiline_code_span_close_toggle_initial_closed.png'
    span_unclosed = 'inline_syntax_boundary/multiline_code_span_close_toggle_unclosed.png'
    span_closed = 'inline_syntax_boundary/multiline_code_span_close_toggle_closed.png'
    _require_image_ref(span_step, 'initial_screenshot', span_initial)
    _require_image_ref(span_step, 'unclosed_screenshot', span_unclosed)
    _require_image_ref(span_step, 'closed_screenshot', span_closed)
    span_unclosed_expected = INLINE_FIXTURE_ORIGINAL.replace('span`', 'span', 1)
    if span_step['unclosed_expected'] != span_unclosed_expected or span_step['unclosed_actual'] != span_unclosed_expected:
        raise ValueError('multiline code span unclosed evidence mismatch')
    if span_step['closed_expected'] != INLINE_FIXTURE_ORIGINAL or span_step['closed_actual'] != INLINE_FIXTURE_ORIGINAL:
        raise ValueError('multiline code span closed evidence mismatch')
    span_body_images = (
        span_initial.removesuffix('.png') + '.body.png',
        span_unclosed.removesuffix('.png') + '.body.png',
        span_closed.removesuffix('.png') + '.body.png',
    )
    span_digests = (span_step['initial_pixel_digest'], span_step['unclosed_pixel_digest'], span_step['closed_pixel_digest'])
    if any(not re.fullmatch('[0-9a-f]{64}', digest) for digest in span_digests):
        raise ValueError('multiline code span body-digest evidence invalid')
    span_artifact_digests = tuple(_artifact_sha256(evidence_dir, image) for image in span_body_images)
    if span_digests != span_artifact_digests:
        raise ValueError('multiline code span body digest does not match artifact')
    if span_step.get('visual_transition_observed') is not True or span_step.get('closed_visual_restored') is not True:
        raise ValueError('multiline code span visual transition evidence missing')
    if span_digests[0] != span_digests[2] or span_digests[1] == span_digests[2]:
        raise ValueError('multiline code span visual transition evidence mismatch')


def validate_receipt(raw, request, evidence_dir, job_conclusion, now=None):
    """Fail closed on provenance/shape errors; never upgrade partial evidence."""
    now = now or datetime.now(timezone.utc)
    if request.get('procedure_version') != PROCEDURE:
        raise ValueError('requested procedure version mismatch')
    expected = (('request_id', request['request_id']), ('run_id', request['run_id']),
                ('run_attempt', request['run_attempt']), ('procedure_version', PROCEDURE))
    for name, value in expected:
        if raw.get(name) != value:
            raise ValueError(f'{name} mismatch')
    if raw.get('control', {}).get('sha') != request['control_sha']:
        raise ValueError('control SHA mismatch')
    target = raw.get('target', {})
    if (target.get('actual_sha') != request['sha'] or target.get('expected_sha') != request['sha']
            or target.get('sha_matches') is not True or target.get('working_copy_clean') is not True):
        if raw.get('overall_result') != 'blocked':
            raise ValueError('target identity or clean-checkout mismatch')
    started, finished = parse_time(raw['started_at']), parse_time(raw['finished_at'])
    if not parse_time(request['created_at']) <= started <= finished <= parse_time(request['expires_at']) or finished > now:
        raise ValueError('result outside request lifetime')
    outcome = raw.get('overall_result')
    if outcome not in ('pass', 'fail', 'blocked'):
        raise ValueError('unknown GUI outcome')
    if job_conclusion not in ('success', 'failure'):
        raise ValueError('worker did not finish normally')
    if outcome == 'pass':
        if job_conclusion != 'success':
            raise ValueError('passing payload from unsuccessful worker')
        build = raw.get('build', {})
        if not re.fullmatch('[0-9a-f]{64}', build.get('binary_sha256', '')) or build.get('features') != ['timing-probe']:
            raise ValueError('missing binary identity or wrong build features')
        if build.get('source_snapshot_sha') != request['sha'] or build.get('source_snapshot_clean') is not True:
            raise ValueError('build input snapshot is not bound to requested SHA')
        runner = raw.get('runner', {})
        if (runner.get('os') != 'macOS' or runner.get('image_os') != 'macos15'
                or {'ARM64': 'arm64', 'X64': 'x86_64'}.get(runner.get('arch')) != runner.get('machine')
                or not isinstance(runner.get('machine'), str)
                or not str(runner.get('macos_version', '')).startswith('15.')
                or not isinstance(runner.get('image_version'), str) or not runner['image_version'].strip()):
            raise ValueError('missing or inconsistent hosted runner image metadata')
        top = raw.get('top_level_steps', [])
        if {s.get('name') for s in top} != {'preflight', 'prepare_helper', 'build'} or any(s.get('result') != 'pass' for s in top):
            raise ValueError('incomplete build/preflight')
        scenarios = raw.get('scenarios', [])
        if len(scenarios) != len(REQUIRED_STEPS) or {s.get('name') for s in scenarios} != set(REQUIRED_STEPS):
            raise ValueError('required scenarios missing or duplicated')
        for scenario in scenarios:
            steps = scenario.get('steps', [])
            if (scenario.get('result') != 'pass' or any(s.get('result') != 'pass' for s in steps)
                    or not REQUIRED_STEPS[scenario['name']] <= {s.get('name') for s in steps}):
                raise ValueError('required steps missing or not passed')
            if scenario['name'] == 'ascii_edit_save_undo_redo_reopen':
                for name in ('launch', 'window_discovery', 'cleanup'):
                    if sum(s.get('name') == name for s in steps) != 2:
                        raise ValueError('reopen lifecycle incomplete')
            if scenario['name'] == 'inline_syntax_boundary':
                _validate_inline_evidence(steps, evidence_dir)
        for relative in REQUIRED_IMAGES:
            _artifact_sha256(evidence_dir, relative)
    return outcome


def receipt(request, outcome, reason, raw=None, evidence_dir=None):
    hashes = {}
    if evidence_dir:
        for path in Path(evidence_dir).rglob('*'):
            if path.is_file() and not path.is_symlink():
                hashes[path.relative_to(evidence_dir).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return {'schema_version': 1, 'policy_version': POLICY, 'request': request,
            'outcome': outcome, 'reason': str(reason)[:2000], 'result': raw, 'sha256': hashes}


def authenticated_receipt(proof, snapshot_sha, pr_number, repository, state, run):
    outcome, generation = state
    run_id, attempt = generation.split('-')
    if not isinstance(proof, dict) or not isinstance(proof.get('request'), dict):
        raise ValueError('invalid GUI receipt shape')
    request = proof['request']
    if (run.get('status') != 'completed' or str(run.get('id')) != run_id
            or str(run.get('run_attempt')) != attempt
            or run.get('path') != '.github/workflows/gui-validation.yml'
            or run.get('head_branch') != 'main'
            or run.get('event') not in ('workflow_dispatch', 'workflow_run', 'schedule')
            or request.get('control_sha') != run.get('head_sha')
            or request.get('procedure_version') != PROCEDURE
            or proof.get('schema_version') != 1 or proof.get('policy_version') != 'v1'
            or request.get('sha') != snapshot_sha or request.get('pr_number') != pr_number
            or request.get('repository') != repository or request.get('generation') != generation
            or request.get('run_id') != run_id or request.get('run_attempt') != attempt
            or request.get('request_id') != f'gui-{generation}-pr{pr_number}'
            or proof.get('outcome') != outcome or outcome == 'pending'):
        raise ValueError('GUI receipt provenance mismatch')
    return proof
