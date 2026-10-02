import importlib.util
import json
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("observation", ROOT / "aadw_gui_observation.py")
observation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observation)


def sample(name="lines_coast", result="pass", times=(20, 50, 200)):
    return {"name": name, "result": result,
            "initial_to_reverse_event_ms": 120,
            "pre_reverse_capture_completed_after_initial_ms": 100,
            "frames": [{"capture_started_elapsed_ms": t - 1, "capture_completed_elapsed_ms": t,
                        "elapsed_ms": t, "event_route": "cghidEventTap", "visible_lines": [2, 3]}
                       for t in times]}


class ObservationTests(unittest.TestCase):
    def test_observed_success_is_preserved(self):
        self.assertEqual(observation.assess_step(sample())["observation"], "observed_pass")

    def test_no_deadline_sample_is_measurement_unavailable_not_product_failure(self):
        result = observation.assess_step(sample(result="fail", times=(90, 140, 200)))
        self.assertEqual(result["failure_class"], "measurement")
        self.assertFalse(result["product_cause_proven"])

    def test_timely_nonresponse_does_not_prove_product_cause(self):
        result = observation.assess_step(sample(result="fail"))
        self.assertEqual(result["observation"], "observed_nonpass")
        self.assertEqual(result["failure_class"], "unknown")

    def test_reversal_outside_window_blocked(self):
        step = sample("direction_reversal"); step["initial_to_reverse_event_ms"] = 140
        self.assertEqual(observation.assess_step(step)["observation"], "unavailable")

    def test_invalid_times_route_rows_or_capture_interval_fail_closed(self):
        changes = [("elapsed_ms", float("nan")), ("capture_started_elapsed_ms", -1),
                   ("capture_completed_elapsed_ms", True), ("event_route", "postToPid"),
                   ("visible_lines", []), ("elapsed_ms", 10), ("visible_lines", [True])]
        for key, value in changes:
            step = sample(); step["frames"][0][key] = value
            with self.subTest(key=key):
                self.assertEqual(observation.assess_step(step)["observation"], "unavailable")

    def test_missing_scenarios_require_diagnosis(self):
        self.assertTrue(observation.assess_report({})["diagnosis_required"])

    def test_all_valid_passes_do_not_require_diagnosis(self):
        report = {"scenarios": [{"steps": [sample(name) for name in observation.TIMED]}]}
        self.assertFalse(observation.assess_report(report)["diagnosis_required"])

    def test_failed_result_cannot_become_pass(self):
        report = {"verification_kind": "scroll_inertia_focused", "overall_result": "fail",
                  "scenarios": [{"steps": [sample(name) for name in observation.TIMED]}]}
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "result.json"; path.write_text(json.dumps(report))
            self.assertEqual(observation.annotate(path)["overall_result"], "fail")

    def test_invalid_pass_becomes_blocked(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "result.json"
            path.write_text(json.dumps({"verification_kind": "scroll_inertia_focused", "overall_result": "pass"}))
            self.assertEqual(observation.annotate(path)["overall_result"], "blocked")

    def test_direct_annotation_preserves_original_judgment(self):
        for result in ("pass", "fail", "blocked"):
            with self.subTest(result=result), tempfile.TemporaryDirectory() as temp:
                original = {"overall_result": result, "overall_reason": "元の判定理由",
                            "summary": "元の要約"}
                source = {"verification_kind": "scroll_inertia_focused", **original,
                          "scenarios": []}
                path = Path(temp) / "result.json"
                path.write_text(json.dumps(source), encoding="utf-8")
                annotated = observation.annotate(path)
                self.assertEqual(annotated["original_judgment"], original)
                self.assertEqual(annotated["scenarios"], source["scenarios"])
                self.assertEqual(annotated["overall_result"], "blocked" if result == "pass" else result)
                self.assertEqual(json.loads(path.read_text(encoding="utf-8")), annotated)

    def test_repeated_annotation_preserves_first_judgment_and_missing_fields(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "result.json"
            path.write_text(json.dumps({"verification_kind": "scroll_inertia_focused",
                                        "overall_result": "pass"}), encoding="utf-8")
            first = observation.annotate(path)
            second = observation.annotate(path)
            self.assertEqual(first["original_judgment"], {"overall_result": "pass"})
            self.assertEqual(second["original_judgment"], first["original_judgment"])
            self.assertEqual(second["overall_result"], "blocked")

    def test_cli_preserves_original_judgment_and_nonzero_exit(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "result.json"
            path.write_text(json.dumps({"verification_kind": "scroll_inertia_focused",
                                        "overall_result": "pass", "summary": "元の要約"}),
                            encoding="utf-8")
            done = subprocess.run([sys.executable, "-I", str(ROOT / "aadw_gui_observation.py"), str(path)],
                                  capture_output=True, text=True)
            self.assertEqual(done.returncode, 1, done.stderr)
            saved = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(saved["overall_result"], "blocked")
            self.assertEqual(saved["original_judgment"], {"overall_result": "pass", "summary": "元の要約"})

    def test_pr395_observed_timing_pattern_is_not_a_fix_authorization(self):
        # Numeric observations from run 36500463393; no OCR is rerun.
        steps = [sample(result="fail", times=(44.336, 74.546, 98.362, 120.951, 136.467, 188.172, 218.162, 292.099)),
                 {"name": "direction_reversal", "result": "blocked"},
                 sample("pixels_direct_follow", "fail", (23.166, 49.064, 68.987, 291.070))]
        result = observation.assess_report({"scenarios": [{"steps": steps}]})
        self.assertTrue(result["diagnosis_required"])
        self.assertEqual([s["failure_class"] for s in result["steps"]], ["unknown", "measurement", "unknown"])
        self.assertFalse(result["product_fix_supported_by_timing_alone"])


if __name__ == "__main__":
    unittest.main()
