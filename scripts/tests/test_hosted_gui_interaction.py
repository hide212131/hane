"""Helper provenance checks without touching a screen or invoking Swift."""
import hashlib
from pathlib import Path
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
