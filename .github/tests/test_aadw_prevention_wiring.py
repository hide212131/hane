"""Integration smoke tests for the trusted entrypoints introduced by #408."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class WiringTests(unittest.TestCase):
    def test_diagnostic_has_no_write_permission_or_product_finalizer(self):
        text = (ROOT / '.github/workflows/aadw-diagnose.yml').read_text()
        self.assertIn('name: AADW Diagnose', text)
        self.assertNotIn('contents: write', text)
        self.assertNotIn('issues: write', text)
        self.assertNotIn('git push', text)
        self.assertNotIn('  finalize:', text)
        self.assertIn('aadw_action_contract.py diagnosis', text)
        self.assertIn('git -C diagnostic-baseline diff', text)
        self.assertLess(text.index('id: claude'), text.index('path: trusted-validator'))
        self.assertIn('ref: ${{ github.workflow_sha }}', text)
        self.assertIn('aadw-claude-fix-pr-', text)

    def test_mutation_keeps_empty_patch_and_stop_guards(self):
        text = (ROOT / '.github/workflows/claude-fix.yml').read_text()
        self.assertIn('aadw_action_contract.py request', text)
        self.assertIn('Claude worker produced no product-code change.', text)
        self.assertGreaterEqual(text.count('<!-- AADW_PAUSED -->'), 2)
        self.assertIn('if [[ "$current_sha" != "$expected_sha" ]]', text)
        self.assertIn('--tools "Read,Edit,Write,Glob,Grep"', text)
        verify = text.split('\n  verify:', 1)[1]
        self.assertNotIn('contents: write', verify)
        self.assertIn('ref: ${{ needs.finalize.outputs.final_sha }}', verify)
        self.assertIn('cargo test --workspace --all-features --locked', verify)
        self.assertIn('cargo clippy --workspace --all-targets --all-features --locked -- -D warnings', verify)
        self.assertIn('test "$TEST_RESULT" = success && test "$LINT_RESULT" = success', verify)
        self.assertIn('if: ${{ !cancelled() }}', verify)

    def test_all_new_contract_tests_are_in_regular_ci(self):
        text = (ROOT / '.github/workflows/ci.yml').read_text()
        for path in ('.github/tests/test_aadw_action_contract.py',
                     '.github/tests/test_aadw_prevention_wiring.py',
                     'scripts/tests/test_aadw_gui_observation.py'):
            self.assertIn('python3 ' + path, text)

    def test_staging_preserves_raw_nonpass_and_adds_measurement_classification(self):
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / 'hane-gui-interaction'
            source.mkdir()
            raw = {'verification_kind': 'scroll_inertia_focused', 'overall_result': 'fail',
                   'scenarios': [{'steps': [{'name': 'direction_reversal', 'result': 'blocked'}]}]}
            (source / 'result.json').write_text(json.dumps(raw))
            (source / 'state').mkdir()
            (source / 'state' / 'result.json').write_text('private-session-state')
            done = subprocess.run([sys.executable, '-I', str(ROOT / '.github/scripts/stage_gui_evidence.py')],
                                  env={**os.environ, 'RUNNER_TEMP': temp}, capture_output=True, text=True)
            self.assertEqual(done.returncode, 0, done.stderr)
            dest = Path(temp) / 'hane-gui-artifact'
            self.assertFalse((dest / 'state').exists())
            self.assertEqual(json.loads((dest / 'raw-result.json').read_text()), raw)
            annotated = json.loads((dest / 'result.json').read_text())
            self.assertEqual(annotated['overall_result'], 'fail')
            self.assertTrue(annotated['observation_quality']['diagnosis_required'])
            self.assertEqual(annotated['observation_quality']['steps'][0]['failure_class'], 'measurement')

    def test_staging_cannot_accept_pass_without_usable_samples(self):
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / 'hane-gui-interaction'; source.mkdir()
            (source / 'result.json').write_text(json.dumps({
                'verification_kind': 'scroll_inertia_focused', 'overall_result': 'pass'}))
            done = subprocess.run([sys.executable, '-I', str(ROOT / '.github/scripts/stage_gui_evidence.py')],
                                  env={**os.environ, 'RUNNER_TEMP': temp}, capture_output=True, text=True)
            self.assertNotEqual(done.returncode, 0)
            dest = Path(temp) / 'hane-gui-artifact'
            self.assertEqual(json.loads((dest / 'result.json').read_text())['overall_result'], 'blocked')


if __name__ == '__main__':
    unittest.main()
