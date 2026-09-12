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
    """Pure source-byte and visible-anchor logic for inline_syntax_boundary."""

    def test_boundary_insertions_match_the_fixture(self):
        text = interaction.INLINE_FIXTURE_ORIGINAL
        cases = [
            (interaction.BOLD_ITALIC_OPEN_RE, 'end', text.replace('**bold', '**Zbold', 1)),
            (interaction.BOLD_ITALIC_CLOSE_RE, 'start', text.replace('combo**', 'comboZ**', 1)),
            (interaction.CODE_SPAN_OPEN_RE, 'end', text.replace('`code', '`Zcode', 1)),
            (interaction.CODE_SPAN_CLOSE_RE, 'start', text.replace('span`', 'spanZ`', 1)),
            (interaction.QUOTE_BOLD_OPEN_RE, 'end', text.replace('quote with **bold', 'quote with **Zbold', 1)),
            (interaction.LIST_ITALIC_OPEN_RE, 'end', text.replace('item with *italic', 'item with *Zitalic', 1)),
        ]
        for pattern, edge, expected in cases:
            with self.subTest(pattern=pattern, edge=edge):
                self.assertEqual(interaction.insert_at_match(text, pattern, 'Z', edge=edge), expected)

    def test_unmatched_pattern_is_rejected_rather_than_silently_skipped(self):
        with self.assertRaises(ValueError):
            interaction.insert_at_match(interaction.INLINE_FIXTURE_ORIGINAL, r'not-present', 'Z', edge='end')

    def test_ocr_locators_match_visible_text_without_hidden_markers(self):
        rendered_lines = [
            'bold italic combo boundary line.',
            'this inline code',
            'span crosses a line.',
            'quote with bold inside.',
            'list item with italic inside.',
        ]
        patterns = [
            interaction.BOLD_ITALIC_OCR_RE,
            interaction.CODE_SPAN_OPEN_OCR_RE,
            interaction.CODE_SPAN_CLOSE_OCR_RE,
            interaction.QUOTE_BOLD_OPEN_OCR_RE,
            interaction.LIST_ITALIC_OPEN_OCR_RE,
            interaction.DRAG_SELECT_START_OCR_RE,
            interaction.DRAG_SELECT_END_OCR_RE,
        ]
        for pattern in patterns:
            with self.subTest(pattern=pattern):
                self.assertEqual(sum(bool(re.search(pattern, line)) for line in rendered_lines), 1)
                self.assertNotRegex(pattern, r'\\[\*`]')

    def test_drag_selection_expected_bytes_cross_hidden_inner_markers(self):
        text = interaction.INLINE_FIXTURE_ORIGINAL
        selected = 'old *italic* com'
        self.assertIn(selected, text)
        self.assertEqual(text.replace(selected, '', 1),
                         text.replace('**bold *italic* combo**', '**bbo**', 1))

    def test_bold_italic_and_quote_source_patterns_target_different_lines(self):
        text = interaction.INLINE_FIXTURE_ORIGINAL
        bold_italic_pos = re.search(interaction.BOLD_ITALIC_OPEN_RE, text).start()
        quote_pos = re.search(interaction.QUOTE_BOLD_OPEN_RE, text).start()
        self.assertNotEqual(bold_italic_pos, quote_pos)
        self.assertIn('*italic*', text[bold_italic_pos:text.index('\n', bold_italic_pos)])
        self.assertIn('quote with', text[:quote_pos].rsplit('\n', 1)[-1])

    def test_delimiter_selection_anchors_are_visible_and_restore_exact_bytes(self):
        unclosed = interaction.INLINE_FIXTURE_ORIGINAL + ' *loose'
        closed = unclosed + '* tail'
        rendered = 'loose tail'
        self.assertIsNotNone(re.search(interaction.DELIMITER_SELECT_START_OCR_RE, rendered))
        self.assertIsNotNone(re.search(interaction.DELIMITER_SELECT_END_OCR_RE, rendered))
        partially_unclosed = interaction.INLINE_FIXTURE_ORIGINAL + ' *l'
        self.assertEqual(partially_unclosed + 'oose* tail', closed)
