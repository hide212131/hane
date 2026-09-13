"""Regression tests for idempotent GUI report reruns."""
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import gui_pipeline as controller
from gui_policy import STATUS_VERSION
from test_gui_controller import FakeGitHub
from test_gui_policy import REQUEST, SHA


class GuiReportRerunTests(unittest.TestCase):
    def test_same_generation_terminal_status_is_preserved(self):
        for outcome, state in (('pass', 'success'), ('fail', 'failure'), ('blocked', 'error')):
            with self.subTest(outcome=outcome):
                api = FakeGitHub()
                api.rows['hane/gui-validation'] = {
                    'state': state,
                    'description': f'GUI {outcome} {STATUS_VERSION} {SHA[:12]} g123-1',
                }
                with patch.object(controller, 'notify') as notify:
                    controller.report(api, REQUEST)
                self.assertEqual(api.writes, [])
                notify.assert_called_once_with(
                    api,
                    REQUEST,
                    'success',
                    detail=f'GUI validation結果: {outcome}',
                )


if __name__ == '__main__':
    unittest.main()
