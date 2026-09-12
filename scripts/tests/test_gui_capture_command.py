"""Regression tests for the native GUI screenshot command."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import gui_validate as gv


class CaptureCommandTests(unittest.TestCase):
    def config(self, root: Path, *, capture_cmd=None) -> gv.Config:
        return gv.Config(
            workspace_dir=root,
            scenario="editor",
            expected_sha=None,
            request_id="capture-command-test",
            generation="1",
            run_dir=root,
            state_dir=root / "state",
            log_path=root / "hane.log",
            image_path=root / "editor.png",
            fixture_path=None,
            features=[],
            extra_env={},
            capture_cmd=capture_cmd,
        )

    def run_capture(self, config: gv.Config):
        calls = []

        def command(argv, **kwargs):
            calls.append(argv)
            if argv[0] in ("screencapture", "fake-capture"):
                config.image_path.write_bytes(b"captured image")
            elif argv[0] == "/usr/bin/sips":
                Path(argv[-1]).write_bytes(b"decoded image")
            return subprocess.CompletedProcess(argv, 0, "", "")

        with patch.object(gv.subprocess, "run", side_effect=command):
            result = gv.RealEnvironment().capture("42", config.image_path, config)
        self.assertTrue(result)
        return calls

    def test_default_capture_omits_window_shadow(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = self.config(Path(tmp))
            calls = self.run_capture(config)
            self.assertEqual(
                calls[0],
                ["screencapture", "-x", "-o", "-l", "42", str(config.image_path)],
            )

    def test_custom_capture_command_contract_is_unchanged(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = self.config(Path(tmp), capture_cmd=["fake-capture", "--flag"])
            calls = self.run_capture(config)
            self.assertEqual(
                calls[0],
                ["fake-capture", "--flag", "42", str(config.image_path)],
            )


if __name__ == "__main__":
    unittest.main()
