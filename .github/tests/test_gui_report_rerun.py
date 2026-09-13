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

    def test_historical_terminal_is_preserved_after_later_generation_or_head_move(self):
        terminal = {
            'id': 41,
            'context': controller.CONTEXT,
            'state': 'success',
            'description': f'GUI pass {STATUS_VERSION} {SHA[:12]} g123-1',
        }
        for changed in ('generation', 'head'):
            with self.subTest(changed=changed):
                api = FakeGitHub()
                if changed == 'generation':
                    api.rows[controller.CONTEXT] = {
                        'id': 42,
                        'state': 'pending',
                        'description': f'GUI pending {STATUS_VERSION} {SHA[:12]} g999-1',
                    }
                else:
                    api.pull['head']['sha'] = 'd' * 40
                with patch.object(api, 'pages', return_value=[terminal]), \
                        patch.object(controller, 'notify') as notify:
                    controller.report(api, REQUEST)
                self.assertEqual(api.writes, [])
                notify.assert_called_once_with(
                    api,
                    REQUEST,
                    'success',
                    detail='GUI validation結果: pass',
                )


if __name__ == '__main__':
    unittest.main()
