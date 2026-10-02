import importlib.util
import json
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest
from unittest.mock import MagicMock, patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("observation", ROOT / "aadw_gui_observation.py")
observation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observation)


def sample(name="lines_coast", result="pass", times=(20, 50, 200)):
    route_fields = {} if name == "direction_reversal" else {"event_route": "cghidEventTap"}
    step = {"name": name, "result": result, "baseline": 1,
            "initial_to_reverse_event_ms": 120,
            "pre_reverse_capture_completed_after_initial_ms": 100,
            "frames": [{"capture_started_elapsed_ms": t - 1, "capture_completed_elapsed_ms": t,
                        "elapsed_ms": t, **route_fields, "visible_lines": [2, 3]}
                       for t in times]}
    if name == "direction_reversal":
        step["event_route"] = "cghidEventTap"
    return step


class ObservationTests(unittest.TestCase):
    def test_observed_success_is_preserved(self):
        self.assertEqual(observation.assess_step(sample())["observation"], "observed_pass")

    def test_reversal_uses_scenario_route_not_frame_route(self):
        self.assertEqual(observation.assess_step(sample("direction_reversal"))["observation"], "observed_pass")
        for route in (None, "postToPid"):
            with self.subTest(route=route):
                step = sample("direction_reversal"); step["event_route"] = route
                for frame in step["frames"]:
                    frame["event_route"] = "cghidEventTap"
                self.assertEqual(observation.assess_step(step)["failure_class"], "measurement")

    def test_normal_scroll_still_requires_each_frame_route(self):
        for name in ("lines_coast", "pixels_direct_follow"):
            with self.subTest(name=name):
                step = sample(name); step["event_route"] = "cghidEventTap"
                del step["frames"][0]["event_route"]
                self.assertEqual(observation.assess_step(step)["failure_class"], "measurement")

    def test_real_producer_capture_and_evaluation_match_observation_contract(self):
        # Exercise the repository's real record construction/evaluation; OS
        # input, capture and OCR are mocked. This is not live GUI acceptance.
        producer_spec = importlib.util.spec_from_file_location("scroll_producer", ROOT / "hosted_scroll_inertia_gui.py")
        producer = importlib.util.module_from_spec(producer_spec)
        producer_spec.loader.exec_module(producer)
        times = (20, 50, 90, 120, 200, 240)
        evidence = {"event_route": "cghidEventTap", "event_post_elapsed_ms": 0,
                    "initial_to_reverse_event_ms": 120,
                    "pre_frame_capture_started_after_initial_ms": [60, 90, 110],
                    "pre_frame_capture_completed_after_initial_ms": [61, 91, 111],
                    "frame_elapsed_ms": list(times),
                    "frame_capture_started_ms": [t - 1 for t in times],
                    "frame_capture_completed_ms": list(times)}
        for name, offsets in (("lines_coast", [3, 6, 8, 9, 10, 10]),
                              ("pixels_direct_follow", [3] * 6),
                              ("direction_reversal", [6, 5, 4, 3, 2, 2])):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temp:
                interaction = MagicMock()
                interaction.run_helper.return_value = (True, "mocked helper timings", None)
                ocr_offsets = ([2, 4, 8] if name == "direction_reversal" else []) + offsets
                with patch.object(producer, "parse_scroll_capture_helper_output", return_value=evidence), \
                     patch.object(producer, "parse_reversal_helper_output", return_value=evidence), \
                     patch.object(producer, "capture_ocr", side_effect=[([n, n + 1], "fixture", None) for n in ocr_offsets]):
                    frames, pre, error = producer.capture_frames(
                        interaction, None, None, None, "unused-helper", 1, "window", Path(temp),
                        "pixels" if name == "pixels_direct_follow" else "lines", -8, times, 1,
                        reverse_delta=12 if name == "direction_reversal" else None, baseline=1)
                self.assertIsNone(error)
                if name == "direction_reversal":
                    self.assertTrue(all("event_route" not in frame for frame in frames))
                    step = producer.evaluate_reversal(1, pre["selected"]["value"], frames,
                        pre["initial_to_reverse_event_ms"], event_route=pre["event_route"],
                        pre_reverse_capture_completed_after_initial_ms=pre["selected"]["capture_completed_after_initial_ms"])
                elif name == "lines_coast":
                    step = producer.evaluate_lines_coast(1, frames)
                else:
                    step = producer.evaluate_pixels(1, frames)
                self.assertEqual(step["result"], "pass", step.get("reason"))
                self.assertEqual(observation.assess_step(step)["observation"], "observed_pass")

    def test_producer_pass_after_80ms_remains_unproven(self):
        for name in ("lines_coast", "pixels_direct_follow"):
            with self.subTest(name=name):
                step = sample(name, times=(20, 50, 120, 200))
                for frame in step["frames"][:2]:
                    frame["visible_lines"] = [1, 2]
                result = observation.assess_step(step)
                self.assertEqual(result["observation"], "observed_nonpass")
                self.assertEqual(result["failure_class"], "unknown")
                self.assertFalse(result["product_cause_proven"])
                report = {"verification_kind": "scroll_inertia_focused",
                          "procedure_version": "hosted-scroll-inertia/8", "overall_result": "pass",
                          "scenarios": [{"steps": [step, *[sample(other) for other in observation.TIMED if other != name]]}]}
                with tempfile.TemporaryDirectory() as temp:
                    path = Path(temp) / "result.json"
                    path.write_text(json.dumps(report), encoding="utf-8")
                    saved = observation.annotate(path)
                    self.assertEqual(saved["overall_result"], "blocked")
                    self.assertEqual(saved["original_judgment"], {"overall_result": "pass"})

    def test_response_at_80ms_is_within_the_unchanged_deadline(self):
        for name in ("lines_coast", "pixels_direct_follow"):
            with self.subTest(name=name):
                step = sample(name, times=(20, 80, 200))
                step["frames"][0]["visible_lines"] = [1, 2]
                self.assertEqual(observation.assess_step(step)["observation"], "observed_pass")

    def test_pass_without_valid_baseline_is_measurement_unavailable(self):
        for baseline in (None, True, 0, 501, "1"):
            with self.subTest(baseline=baseline):
                step = sample(); step["baseline"] = baseline
                self.assertEqual(observation.assess_step(step)["failure_class"], "measurement")

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
