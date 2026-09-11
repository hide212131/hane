"""Helper provenance checks without touching a screen or invoking Swift."""
import hashlib
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import hosted_gui_interaction as interaction


class HelperTests(unittest.TestCase):
    def test_target_source_replacement_is_not_executed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'helper.swift'
            source.write_text('trusted source')
            def compile_helper(argv, **kwargs):
                Path(argv[-1]).write_bytes(b'trusted executable')
            with patch.object(interaction.subprocess, 'run', side_effect=compile_helper):
                helper = interaction.prepare_helper(source, root)
            source.write_text('target build.rs replaced source')
            with patch.object(interaction.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, 'ok', '')) as execute:
                self.assertTrue(interaction.run_helper(helper, ['ocr'], 1)[0])
                self.assertEqual(execute.call_args.args[0], [str(helper.binary), 'ocr'])

    def test_binary_replacement_before_or_during_execution_is_blocked(self):
        for when in ('before', 'during'):
            with self.subTest(when=when), tempfile.TemporaryDirectory() as directory:
                binary = Path(directory) / 'helper'
                binary.write_bytes(b'trusted')
                helper = interaction.PreparedHelper(binary, hashlib.sha256(b'trusted').hexdigest())
                if when == 'before':
                    binary.write_bytes(b'replaced')
                def execute(*args, **kwargs):
                    binary.write_bytes(b'replaced')
                    return subprocess.CompletedProcess([], 0, 'forged pass', '')
                with patch.object(interaction.subprocess, 'run', side_effect=execute) as run:
                    ok, output, reason = interaction.run_helper(helper, ['ocr'], 1)
                    self.assertFalse(ok)
                    self.assertEqual(output, '')
                    self.assertIn('integrity mismatch', reason)
                    self.assertEqual(run.call_count, int(when == 'during'))


class InlineSyntaxExpectationTests(unittest.TestCase):
    """Pure byte-expectation logic for the inline_syntax_boundary scenario.

    These patterns also drive the real OS-level click in
    hosted_gui_interaction.swift (via OCR); this test pins their meaning
    against the fixture text without touching a screen or Swift.
    """

    def test_boundary_insertions_match_the_fixture(self):
        text = interaction.INLINE_FIXTURE_ORIGINAL
        cases = [
            (interaction.BOLD_ITALIC_OPEN_RE, 'end', text.replace('**bold', '**Zbold', 1)),
            (interaction.BOLD_ITALIC_CLOSE_RE, 'start', text.replace('combo**', 'comboZ**', 1)),
            (interaction.CODE_SPAN_OPEN_RE, 'end', text.replace('`code', '`Zcode', 1)),
            (interaction.QUOTE_BOLD_OPEN_RE, 'end', text.replace('quote with **bold', 'quote with **Zbold', 1)),
            (interaction.LIST_ITALIC_OPEN_RE, 'end', text.replace('item with *italic', 'item with *Zitalic', 1)),
        ]
        for pattern, edge, expected in cases:
            with self.subTest(pattern=pattern, edge=edge):
                self.assertEqual(interaction.insert_at_match(text, pattern, 'Z', edge=edge), expected)

    def test_unmatched_pattern_is_rejected_rather_than_silently_skipped(self):
        with self.assertRaises(ValueError):
            interaction.insert_at_match(interaction.INLINE_FIXTURE_ORIGINAL, r'not-present', 'Z', edge='end')

    def test_drag_select_markers_bound_exactly_the_inner_italic_run(self):
        text = interaction.INLINE_FIXTURE_ORIGINAL
        start = re.search(interaction.DRAG_SELECT_START_RE, text).start()
        end = re.search(interaction.DRAG_SELECT_END_RE, text).end()
        self.assertEqual(text[start:end], '*italic*')
        self.assertEqual(text.replace('*italic*', '', 1),
                         text[:start] + text[end:])

    def test_bold_italic_and_quote_bold_patterns_target_different_lines(self):
        # "**bold" appears on both the combo line and the quote line; the
        # lookahead context must keep the two patterns from colliding.
        text = interaction.INLINE_FIXTURE_ORIGINAL
        bold_italic_pos = re.search(interaction.BOLD_ITALIC_OPEN_RE, text).start()
        quote_pos = re.search(interaction.QUOTE_BOLD_OPEN_RE, text).start()
        self.assertNotEqual(bold_italic_pos, quote_pos)
        self.assertIn('*italic*', text[bold_italic_pos:text.index('\n', bold_italic_pos)])
        self.assertIn('quote with', text[:quote_pos].rsplit('\n', 1)[-1])

    def test_delimiter_close_pattern_matches_only_the_appended_closing_marker(self):
        unclosed = interaction.INLINE_FIXTURE_ORIGINAL + ' *loose'
        closed = unclosed + '*'
        self.assertIsNone(re.search(interaction.DELIMITER_CLOSE_RE, unclosed))
        match = re.search(interaction.DELIMITER_CLOSE_RE, closed)
        self.assertIsNotNone(match)
        self.assertEqual(closed[match.start():match.end()], '*')
        self.assertEqual(match.end(), len(closed))
