"""Regression tests for idempotent GUI report reruns."""
import json
import os
from pathlib import Path
import tempfile
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import gui_pipeline as controller
from gui_policy import STATUS_VERSION
from test_gui_controller import FakeGitHub
from test_gui_policy import REQUEST, SHA


class GuiReportRerunTests(unittest.TestCase):
    def test_same_generation_terminal_status_is_preserved_and_receipt_restored(self):
        for outcome, state in (('pass', 'success'), ('fail', 'failure'), ('blocked', 'error')):
            with self.subTest(outcome=outcome), tempfile.TemporaryDirectory() as temp:
                api = FakeGitHub()
                api.rows['hane/gui-validation'] = {
                    'state': state,
                    'description': f'GUI {outcome} {STATUS_VERSION} {SHA[:12]} g123-1',
                }
                path = Path(temp) / 'gui-receipt.json'
                proof = {'schema_version': 1, 'outcome': outcome}
                with patch.dict(os.environ, {'RECEIPT_PATH': str(path)}), \
                        patch.object(controller, 'report_receipt', return_value=(outcome, proof)) as rebuild, \
                        patch.object(controller, 'notify') as notify:
                    controller.report(api, REQUEST)
                self.assertEqual(api.writes, [])
                self.assertEqual(json.loads(path.read_text()), proof)
                rebuild.assert_called_once_with(api, REQUEST)
                notify.assert_called_once_with(
                    api,
                    REQUEST,
                    'success',
                    detail=f'GUI validation結果: {outcome}',
                )

    def test_historical_terminal_is_preserved_and_receipt_restored_after_later_generation_or_head_move(self):
        terminal = {
            'id': 41,
            'context': controller.CONTEXT,
            'state': 'success',
            'description': f'GUI pass {STATUS_VERSION} {SHA[:12]} g123-1',
        }
        for changed in ('generation', 'head'):
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as temp:
                api = FakeGitHub()
                if changed == 'generation':
                    api.rows[controller.CONTEXT] = {
                        'id': 42,
                        'state': 'pending',
                        'description': f'GUI pending {STATUS_VERSION} {SHA[:12]} g999-1',
                    }
                else:
                    api.pull['head']['sha'] = 'd' * 40
                path = Path(temp) / 'gui-receipt.json'
                proof = {'schema_version': 1, 'outcome': 'pass'}
                with patch.dict(os.environ, {'RECEIPT_PATH': str(path)}), \
                        patch.object(api, 'pages', return_value=[terminal]), \
                        patch.object(controller, 'report_receipt', return_value=('pass', proof)) as rebuild, \
                        patch.object(controller, 'notify') as notify:
                    controller.report(api, REQUEST)
                self.assertEqual(api.writes, [])
                self.assertEqual(json.loads(path.read_text()), proof)
                rebuild.assert_called_once_with(api, REQUEST)
                notify.assert_called_once_with(
                    api,
                    REQUEST,
                    'success',
                    detail='GUI validation結果: pass',
                )

    def test_terminal_receipt_restore_fails_closed_on_outcome_mismatch(self):
        api = FakeGitHub()
        api.rows['hane/gui-validation'] = {
            'state': 'success',
            'description': f'GUI pass {STATUS_VERSION} {SHA[:12]} g123-1',
        }
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'gui-receipt.json'
            with patch.dict(os.environ, {'RECEIPT_PATH': str(path)}), \
                    patch.object(controller, 'report_receipt', return_value=('fail', {'outcome': 'fail'})), \
                    patch.object(controller, 'notify') as notify:
                with self.assertRaisesRegex(ValueError, 'does not match reconstructed receipt'):
                    controller.report(api, REQUEST)
            self.assertFalse(path.exists())
            self.assertEqual(api.writes, [])
            notify.assert_not_called()


if __name__ == '__main__':
    unittest.main()
