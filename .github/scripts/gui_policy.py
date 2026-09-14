"""Pure GUI request/receipt policy. No network, tokens, or target execution."""
from datetime import datetime, timezone
import hashlib
import json
import math
from pathlib import Path
import re

POLICY = 'v1'
PROCEDURE = 'hosted-gui-interaction/7'
STATUS_VERSION = f'{POLICY}-p{PROCEDURE.rsplit("/", 1)[1]}'
CONTEXT = 'hane/gui-validation'
STATUS = re.compile(r'GUI (pending|pass|fail|blocked) ' + re.escape(STATUS_VERSION) + r' ([0-9a-f]{12}) g([0-9]+-[0-9]+)')
BOUNDARY_MARK = 'Z'
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
    'coordinate_independent_probe': {'coordinate_independent_probe'},
}
# scripts/hosted_gui_interaction.py の COORDINATE_PROBE_TEST_QUALIFIED_NAME と同じ値。
# 独立 probe が実際にこの1件の hit-test を実行して pass したことを、cargo test の
# 生出力から突き合わせて確認するために使う(PR #139 review: `result == "pass"` だけでは
# hit-test 未実行や証拠欠落の receipt も通ってしまうため)。
COORDINATE_PROBE_TEST_QUALIFIED_NAME = 'view::tests::boundary_click_lands_on_source_offset_independent_of_ocr'
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


def _numeric(mapping, *fields):
    for field in fields:
        value = mapping.get(field) if isinstance(mapping, dict) else None
        if not isinstance(value, (int, float)) or isinstance(value, bool) or not math.isfinite(value):
            raise ValueError(f'click evidence field not numeric: {field}')


def _require_click_evidence(step, expected_text, expected_edge):
    evidence = step.get('click_evidence')
    if not isinstance(evidence, dict):
        raise ValueError('missing click-text OCR/click evidence')
    required = ('matched_text', 'bounding_box', 'window_bounds', 'click_point', 'edge')
    for field in required:
        if field not in evidence:
            raise ValueError(f'click evidence missing field: {field}')
    if not isinstance(evidence['matched_text'], str) or not evidence['matched_text']:
        raise ValueError('click evidence matched_text invalid')
    if evidence['matched_text'] != expected_text:
        raise ValueError('click evidence matched_text does not match expected boundary target')
    if evidence['edge'] not in ('start', 'end'):
        raise ValueError('click evidence edge invalid')
    if evidence['edge'] != expected_edge:
        raise ValueError('click evidence edge does not match expected boundary side')

    box = evidence['bounding_box']
    if not isinstance(box, dict):
        raise ValueError('click evidence bounding_box invalid')
    _numeric(box, 'minX', 'maxX', 'minY', 'maxY')
    if not (0 <= box['minX'] < box['maxX'] <= 1 and 0 <= box['minY'] < box['maxY'] <= 1):
        raise ValueError('click evidence bounding_box out of normalized range')

    window = evidence['window_bounds']
    if not isinstance(window, dict):
        raise ValueError('click evidence window_bounds invalid')
    _numeric(window, 'x', 'y', 'width', 'height')
    if window['width'] <= 0 or window['height'] <= 0:
        raise ValueError('click evidence window_bounds out of range')

    point = evidence['click_point']
    if not isinstance(point, dict):
        raise ValueError('click evidence click_point invalid')
    _numeric(point, 'x', 'y')

    x_norm = box['minX'] if evidence['edge'] == 'start' else box['maxX']
    expected_x = window['x'] + x_norm * window['width']
    y_norm_from_top = 1 - (box['minY'] + (box['maxY'] - box['minY']) / 2)
    expected_y = window['y'] + y_norm_from_top * window['height']
    tolerance = 1e-6 * max(1.0, window['width'], window['height'])
    if abs(point['x'] - expected_x) > tolerance or abs(point['y'] - expected_y) > tolerance:
        raise ValueError('click evidence click_point does not match bounding_box/edge geometry')


def _require_boundary_landing(step, expected_text, expected_edge, expected_canonical_offset):
    _require_click_evidence(step, expected_text, expected_edge)
    expected_offset = step.get('expected_canonical_source_offset')
    actual_offset = step.get('actual_landing_source_offset')
    if not isinstance(expected_offset, int) or isinstance(expected_offset, bool) or expected_offset < 0:
        raise ValueError('missing or invalid expected canonical source offset')
    if not isinstance(actual_offset, int) or isinstance(actual_offset, bool) or actual_offset < 0:
        raise ValueError('missing or invalid actual landing source offset')
    if expected_offset != expected_canonical_offset:
        raise ValueError('expected canonical source offset does not match known fixture position')
    if step.get('landing_classification') != 'at_canonical' or actual_offset != expected_offset:
        raise ValueError('boundary landing classification/offset evidence mismatch')


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
    # OCR がマッチとして返す文字列は lookaround の zero-width 部分を含まないため、
    # ここでの期待値は hosted_gui_interaction.py の *_OCR_RE から lookaround を除いた
    # 実際にキャプチャされるリテラル文字列にする。
    boundary_click_edit_checks = {
        'boundary_click_edit_bold_italic_check_0': ('bold italic combo', 'start'),
        'boundary_click_edit_bold_italic_check_1': ('bold italic combo', 'end'),
        'boundary_click_edit_code_span_check_0': ('code', 'start'),
        'boundary_click_edit_code_span_check_1': ('span', 'end'),
        'boundary_click_edit_quote_check_0': ('bold', 'start'),
        'boundary_click_edit_list_check_0': ('italic', 'start'),
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
        if name in boundary_click_edit_checks:
            expected_pattern, expected_edge = boundary_click_edit_checks[name]
            expected_canonical_offset = boundary_expected[name].index(BOUNDARY_MARK)
            _require_boundary_landing(step, expected_pattern, expected_edge, expected_canonical_offset)

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


# scripts/hosted_gui_interaction.py の COORDINATE_PROBE_RUST_SOURCE に注入される
# `#[gpui::test]` が使う source text と完全に同じ literal(PR #139 review: 各ケースの
# canonical offset を、probe が自己申告する値ではなく source bytes から独立に導出
# して突き合わせるため)。
COORDINATE_PROBE_SOURCE_TEXT = (
    'x\n\n**bold *italic* combo** boundary line.\n\n'
    'this inline `code\nspan` crosses a line.\n\n'
    '> quote with **bold**\n\n'
    '- list item with *italic*\n'
)


def _coordinate_probe_expected_cases():
    text = COORDINATE_PROBE_SOURCE_TEXT

    bold_source_line = '**bold *italic* combo** boundary line.'
    bold_line_start = text.index(bold_source_line)
    bold_open = bold_line_start + 2
    bold_close = bold_line_start + bold_source_line.index('combo**') + len('combo')

    code_open_source_line = 'this inline `code'
    code_open_line_start = text.index(code_open_source_line)
    code_open = code_open_line_start + code_open_source_line.index('`') + 1

    code_close_source_line = 'span` crosses a line.'
    code_close_line_start = text.index(code_close_source_line)
    code_close = code_close_line_start + code_close_source_line.index('span`') + len('span')

    quote_source_line = '> quote with **bold**'
    quote_line_start = text.index(quote_source_line)
    quote_open = quote_line_start + quote_source_line.index('**bold') + 2
    quote_close = quote_line_start + quote_source_line.index('bold**') + len('bold')

    list_source_line = '- list item with *italic*'
    list_line_start = text.index(list_source_line)
    list_open = list_line_start + list_source_line.index('*italic') + 1
    list_close = list_line_start + list_source_line.index('italic*') + len('italic')

    return {
        'bold_open': ('start', bold_open), 'bold_close': ('end', bold_close),
        'code_open': ('start', code_open), 'code_close': ('end', code_close),
        'quote_open': ('start', quote_open), 'quote_close': ('end', quote_close),
        'list_open': ('start', list_open), 'list_close': ('end', list_close),
    }


def _validate_probe_common_evidence(step, expected_target_test_line):
    if step.get('restored') is not True or step.get('clean_tree') is not True:
        raise ValueError('coordinate-independent probe restore/clean-tree evidence missing')
    if step.get('clean_tree_paths') != []:
        raise ValueError('coordinate-independent probe clean-tree evidence missing or not empty')
    original_sha256 = step.get('original_view_rs_sha256')
    restored_sha256 = step.get('restored_view_rs_sha256')
    if not isinstance(original_sha256, str) or not re.fullmatch('[0-9a-f]{64}', original_sha256):
        raise ValueError('coordinate-independent probe original view.rs SHA-256 missing or invalid')
    if original_sha256 != restored_sha256:
        raise ValueError('coordinate-independent probe restored view.rs does not hash-match the original')
    if step.get('test_name') != COORDINATE_PROBE_TEST_QUALIFIED_NAME:
        raise ValueError('coordinate-independent probe test name mismatch')
    if step.get('tests_executed') != 1:
        raise ValueError('coordinate-independent probe did not execute exactly one hit-test')
    output = step.get('cargo_test_output')
    if not isinstance(output, str) or not output.strip():
        raise ValueError('coordinate-independent probe cargo test output missing')
    if step.get('target_test_line') != expected_target_test_line:
        raise ValueError('coordinate-independent probe cargo test output does not confirm target test result')


def _validate_probe_case_offset(case, case_id, expected_edge, expected_offset):
    if case.get('edge') != expected_edge:
        raise ValueError(f'coordinate-independent probe case {case_id} boundary side mismatch')
    canonical_offset = case.get('canonical_source_offset')
    if (not isinstance(canonical_offset, int) or isinstance(canonical_offset, bool)
            or canonical_offset != expected_offset):
        raise ValueError(f'coordinate-independent probe case {case_id} canonical offset mismatch')
    actual_offset = case.get('actual_source_offset')
    max_offset = len(COORDINATE_PROBE_SOURCE_TEXT.encode('utf-8'))
    if (not isinstance(actual_offset, int) or isinstance(actual_offset, bool)
            or not 0 <= actual_offset <= max_offset):
        raise ValueError(f'coordinate-independent probe case {case_id} actual offset missing or out of source range')
    return actual_offset


def _iter_probe_cases(cases, expected_cases):
    if not isinstance(cases, list) or len(cases) != len(expected_cases):
        raise ValueError('coordinate-independent probe per-case evidence missing or incomplete')
    seen = set()
    for case in cases:
        if not isinstance(case, dict):
            raise ValueError('coordinate-independent probe case evidence malformed')
        case_id = case.get('case')
        if case_id not in expected_cases or case_id in seen:
            raise ValueError('coordinate-independent probe case identity missing or duplicated')
        seen.add(case_id)
        expected_edge, expected_offset = expected_cases[case_id]
        yield case_id, expected_edge, expected_offset, case
    if seen != set(expected_cases):
        raise ValueError('coordinate-independent probe missing expected case coverage')


def _validate_coordinate_probe_evidence(steps):
    step = next((s for s in steps if s.get('name') == 'coordinate_independent_probe'), None)
    if step is None:
        raise ValueError('coordinate-independent probe step missing')
    _validate_probe_common_evidence(step, f'test {COORDINATE_PROBE_TEST_QUALIFIED_NAME} ... ok')

    expected_cases = _coordinate_probe_expected_cases()
    cases = step.get('probe_cases')
    for case_id, expected_edge, expected_offset, case in _iter_probe_cases(cases, expected_cases):
        actual_offset = _validate_probe_case_offset(case, case_id, expected_edge, expected_offset)
        if actual_offset != expected_offset:
            raise ValueError(f'coordinate-independent probe case {case_id} landed on a non-canonical offset')
        if case.get('classification') != 'at_canonical':
            raise ValueError(f'coordinate-independent probe case {case_id} classification is not at_canonical')


def _validate_coordinate_probe_fail_evidence(steps):
    """`overall_result='fail'` の独立 probe 専用契約(Issue #137 review, PR #139)。

    `if outcome == 'pass'` の外側で fail/blocked が丸ごと未検証のまま受理されると、
    case identity・edge・canonical/actual offset・classification・復元 hash・
    dirty-tree proof が欠落・改変された fail receipt も final judge に通ってしまう
    ため、helper/OCR/環境障害由来の blocked とは分離して fail 専用に必須化する。

    この専用契約は `coordinate_independent_probe` scenario 自身が
    `result == 'fail'` を自己申告した場合にのみ適用する(PR #139 review)。
    ascii_edit_save_undo_redo_reopen・japanese_ime_input・os_scroll・
    inline_syntax_boundary など他 scenario の製品不具合による正当な
    overall_result='fail' まで、probe 自身の fail を一律には要求しない。
    """
    step = next((s for s in steps if s.get('name') == 'coordinate_independent_probe'), None)
    if step is None:
        raise ValueError('coordinate-independent probe fail step missing')
    if step.get('result') != 'fail':
        raise ValueError('coordinate-independent probe scenario fail is not backed by a fail step')
    _validate_probe_common_evidence(step, f'test {COORDINATE_PROBE_TEST_QUALIFIED_NAME} ... FAILED')

    expected_cases = _coordinate_probe_expected_cases()
    cases = step.get('probe_cases')
    mismatch_found = False
    for case_id, expected_edge, expected_offset, case in _iter_probe_cases(cases, expected_cases):
        actual_offset = _validate_probe_case_offset(case, case_id, expected_edge, expected_offset)
        is_mismatch = actual_offset != expected_offset
        if case.get('classification') != ('mismatch' if is_mismatch else 'at_canonical'):
            raise ValueError(f'coordinate-independent probe case {case_id} classification inconsistent with offsets')
        mismatch_found = mismatch_found or is_mismatch
    if not mismatch_found:
        raise ValueError('coordinate-independent probe fail evidence has no non-canonical mismatch case')


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
    if outcome == 'fail':
        scenarios = raw.get('scenarios', [])
        probe_scenario = next((s for s in scenarios if s.get('name') == 'coordinate_independent_probe'), None)
        if probe_scenario is not None and probe_scenario.get('result') == 'fail':
            _validate_coordinate_probe_fail_evidence(probe_scenario.get('steps', []))
        elif not any(s.get('result') == 'fail' for s in raw.get('top_level_steps', []) + scenarios):
            raise ValueError('fail outcome is not backed by any scenario or step reporting its own fail')
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
            if scenario['name'] == 'coordinate_independent_probe':
                _validate_coordinate_probe_evidence(steps)
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
