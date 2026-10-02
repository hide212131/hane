"""Integration smoke tests for the trusted entrypoints introduced by #408."""
import importlib.util
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

    def test_existing_reversal_contract_checks_no_old_coast_before_55ms(self):
        # Keep the existing producer contract: no old coast in the first
        # <=55ms observation; reversed direction in the final observation.
        modules = []
        for name in ("hosted_scroll_inertia_gui", "aadw_gui_observation"):
            spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / (name + ".py"))
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            modules.append(module)
        producer, observer = modules
        frames = [{"capture_started_elapsed_ms": t - 1, "capture_completed_elapsed_ms": t,
                   "elapsed_ms": t, "visible_lines": [offset, offset + 1]}
                  for t, offset in zip((20, 50, 120, 200), (8, 8, 7, 6))]
        passed = producer.evaluate_reversal(1, 8, frames, 120,
            event_route="cghidEventTap", pre_reverse_capture_completed_after_initial_ms=100)
        self.assertEqual(passed["result"], "pass")
        self.assertEqual(observer.assess_step(passed)["observation"], "observed_pass")
        frames[0]["visible_lines"] = [10, 11]
        failed = producer.evaluate_reversal(1, 8, frames, 120,
            event_route="cghidEventTap", pre_reverse_capture_completed_after_initial_ms=100)
        self.assertEqual(failed["result"], "fail")
        self.assertEqual(observer.assess_step(failed)["observation"], "observed_nonpass")

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
            context = b'{"requested_head_sha": "test-head", "workflow_run_id": "test-run"}\n'
            (source / 'aadw-context.json').write_bytes(context)
            (source / 'result.json').write_text(json.dumps({
                'verification_kind': 'scroll_inertia_focused', 'overall_result': 'pass'}))
            done = subprocess.run([sys.executable, '-I', str(ROOT / '.github/scripts/stage_gui_evidence.py')],
                                  env={**os.environ, 'RUNNER_TEMP': temp}, capture_output=True, text=True)
            self.assertNotEqual(done.returncode, 0)
            dest = Path(temp) / 'hane-gui-artifact'
            self.assertEqual(json.loads((dest / 'result.json').read_text())['overall_result'], 'blocked')
            self.assertEqual((dest / 'aadw-context.json').read_bytes(), context)
            self.assertEqual(json.loads((dest / 'raw-result.json').read_text())['overall_result'], 'pass')


    def test_staging_keeps_context_when_result_json_is_invalid(self):
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / 'hane-gui-interaction'; source.mkdir()
            context = b'{"requested_head_sha": "test-head", "workflow_run_id": "test-run"}\n'
            (source / 'aadw-context.json').write_bytes(context)
            (source / 'result.json').write_text('{invalid-json', encoding='utf-8')
            done = subprocess.run([sys.executable, '-I', str(ROOT / '.github/scripts/stage_gui_evidence.py')],
                                  env={**os.environ, 'RUNNER_TEMP': temp}, capture_output=True, text=True)
            self.assertNotEqual(done.returncode, 0)
            dest = Path(temp) / 'hane-gui-artifact'
            self.assertEqual((dest / 'aadw-context.json').read_bytes(), context)
            self.assertEqual((dest / 'result.json').read_text(), '{invalid-json')

    def test_staging_context_is_root_only_and_not_a_symlink(self):
        for linked in (False, True):
            with self.subTest(linked=linked), tempfile.TemporaryDirectory() as temp:
                source = Path(temp) / 'hane-gui-interaction'; source.mkdir()
                (source / 'state').mkdir()
                private_context = source / 'state' / 'aadw-context.json'
                private_context.write_text('private-session-state', encoding='utf-8')
                if linked:
                    (source / 'aadw-context.json').symlink_to(private_context)
                done = subprocess.run([sys.executable, '-I', str(ROOT / '.github/scripts/stage_gui_evidence.py')],
                                      env={**os.environ, 'RUNNER_TEMP': temp}, capture_output=True, text=True)
                self.assertEqual(done.returncode, 0, done.stderr)
                dest = Path(temp) / 'hane-gui-artifact'
                self.assertFalse((dest / 'aadw-context.json').exists())
                self.assertFalse((dest / 'state').exists())


if __name__ == '__main__':
    unittest.main()
