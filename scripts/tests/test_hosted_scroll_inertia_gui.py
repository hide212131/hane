#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest

MODULE_PATH = Path(__file__).resolve().parents[1] / "hosted_scroll_inertia_gui.py"
SWIFT_HELPER_PATH = Path(__file__).resolve().parents[1] / "hosted_gui_interaction.swift"
SPEC = importlib.util.spec_from_file_location("hosted_scroll_inertia_gui", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
gui = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gui)


def frames(offsets, times):
    return [
        {"visible_lines": [offset, offset + 1, offset + 2], "elapsed_ms": elapsed}
        for offset, elapsed in zip(offsets, times)
    ]


class VisibleLinesTests(unittest.TestCase):
    def test_extracts_sorted_unique_line_numbers(self):
        self.assertEqual(gui.visible_lines("LINE 010\nline 2\nLINE 010"), [2, 10])

    def test_ignores_non_fixture_text(self):
        self.assertEqual(gui.visible_lines("line one\nLINE xyz\n"), [])


class CaptureSingleTests(unittest.TestCase):
    def test_ocr_failure_marks_capture_blocked_with_reason(self):
        class FailingOcrInteraction:
            def capture_named(self, *_args):
                return {"result": "pass"}

            def run_helper(self, *_args):
                return False, "", "OCR helper unavailable"

        capture, visible, recognized = gui.capture_single(
            FailingOcrInteraction(), None, None, None, None, "window",
            Path(__file__).parent, "baseline", 1.0,
        )

        self.assertEqual(capture["result"], "blocked")
        self.assertIn("OCR helper unavailable", capture["reason"])
        self.assertIsNone(visible)
        self.assertEqual(recognized, "OCR helper unavailable")


class LinesCoastTests(unittest.TestCase):
    def test_accepts_immediate_coast_deceleration_and_settling(self):
        result = gui.evaluate_lines_coast(
            100,
            frames([104, 106, 109, 111, 112, 113, 113, 113],
                   [8, 24, 48, 72, 108, 144, 190, 240]),
        )
        self.assertEqual(result["result"], "pass")
        self.assertTrue(result["first_response"])
        self.assertTrue(result["continued_after_release"])
        self.assertTrue(result["decelerated"])
        self.assertTrue(result["settled"])

    def test_accepts_first_visible_response_within_threshold_after_baseline_frame(self):
        result = gui.evaluate_lines_coast(
            100,
            frames([100, 104, 106, 109, 112, 113, 113, 113],
                   [0, 24, 48, 72, 108, 144, 190, 240]),
        )
        self.assertEqual(result["result"], "pass")
        self.assertTrue(result["first_response"])
        self.assertEqual(result["first_response_frame"], 1)

    def test_rejects_missing_initial_response(self):
        result = gui.evaluate_lines_coast(
            100,
            frames([100, 101, 102, 103, 104, 104, 104, 104],
                   [100, 124, 148, 172, 208, 244, 290, 340]),
        )
        self.assertEqual(result["result"], "fail")
        self.assertFalse(result["first_response"])

    def test_rejects_missing_afterglow(self):
        result = gui.evaluate_lines_coast(
            100,
            frames([108, 108, 108, 108, 108, 108, 108, 108],
                   [8, 24, 48, 72, 108, 144, 190, 240]),
        )
        self.assertEqual(result["result"], "fail")
        self.assertFalse(result["continued_after_release"])


class DirectionReversalTests(unittest.TestCase):
    def test_accepts_prompt_opposite_direction(self):
        result = gui.evaluate_reversal(100, 108, frames([107, 105, 102, 100, 99, 99],
                                        [5, 24, 48, 80, 120, 180]),
                                        initial_to_reverse_event_ms=90,
                                        event_route="cghidEventTap",
                                        pre_reverse_capture_completed_after_initial_ms=70)
        self.assertEqual(result["result"], "pass")
        self.assertEqual(result["event_route"], "cghidEventTap")
        self.assertTrue(result["old_direction_started"])
        self.assertTrue(result["prompt"])
        self.assertTrue(result["reversed_direction"])
        self.assertTrue(result["no_old_coast"])

    def test_rejects_old_direction_after_reversal(self):
        result = gui.evaluate_reversal(100, 108, frames([110, 109, 106, 104, 103, 103],
                                        [5, 24, 48, 80, 120, 180]),
                                        initial_to_reverse_event_ms=90,
                                        pre_reverse_capture_completed_after_initial_ms=70)
        self.assertEqual(result["result"], "fail")
        self.assertFalse(result["prompt"])
        self.assertFalse(result["no_old_coast"])

    def test_blocks_when_capture_delays_reversal_past_inertia_window(self):
        result = gui.evaluate_reversal(
            100, 108, frames([107, 105, 102, 100, 99, 99], [5, 24, 48, 80, 120, 180]),
            initial_to_reverse_event_ms=136,
            pre_reverse_capture_completed_after_initial_ms=100,
        )
        self.assertEqual(result["result"], "blocked")
        self.assertEqual(result["inertia_window_ms"], gui.LINES_INERTIA_WINDOW_MS)

    def test_blocks_when_reversal_timing_is_unavailable(self):
        result = gui.evaluate_reversal(
            100, 108, frames([107, 105, 102, 100, 99, 99], [5, 24, 48, 80, 120, 180]),
            initial_to_reverse_event_ms=None,
            pre_reverse_capture_completed_after_initial_ms=90,
        )
        self.assertEqual(result["result"], "blocked")

    def test_blocks_when_old_direction_capture_finishes_after_inertia_window(self):
        result = gui.evaluate_reversal(
            100, 108, frames([107, 105, 102, 100], [5, 24, 48, 80]),
            initial_to_reverse_event_ms=90,
            pre_reverse_capture_completed_after_initial_ms=136,
        )
        self.assertEqual(result["result"], "blocked")


class ReversalHelperTimingTests(unittest.TestCase):
    def test_parses_event_interval_and_frame_times(self):
        evidence = gui.parse_reversal_helper_output(
            "initial_event_elapsed_ms=2.000\n"
            "pre_reverse_capture_started_elapsed_ms=36.500\n"
            "pre_reverse_capture_completed_elapsed_ms=37.500\n"
            "pre_reverse_visible_lines=100,108\n"
            "reversal_event_route=cghidEventTap\n"
            "reverse_event_elapsed_ms=40.250\n"
            "frame_00_capture_started_ms=4.100\n"
            "frame_00_capture_completed_ms=5.100\n"
            "frame_01_capture_started_ms=25.000\n"
            "frame_01_capture_completed_ms=26.000\n",
            expected_frames=2,
        )
        self.assertEqual(evidence["event_route"], "cghidEventTap")
        self.assertEqual(evidence["initial_to_reverse_event_ms"], 38.25)
        self.assertEqual(evidence["pre_reverse_capture_after_initial_ms"], 34.5)
        self.assertEqual(evidence["pre_reverse_capture_completed_after_initial_ms"], 35.5)
        self.assertEqual(evidence["frame_elapsed_ms"], [5.1, 26.0])
        self.assertEqual(evidence["frame_capture_started_ms"], [4.1, 25.0])
        self.assertEqual(evidence["frame_capture_completed_ms"], [5.1, 26.0])

    def test_rejects_out_of_order_pre_reversal_capture(self):
        with self.assertRaisesRegex(ValueError, "out of order"):
            gui.parse_reversal_helper_output(
                "initial_event_elapsed_ms=2\n"
                "pre_reverse_capture_started_elapsed_ms=8\n"
                "pre_reverse_capture_completed_elapsed_ms=9\n"
                "reversal_event_route=cghidEventTap\n"
                "reverse_event_elapsed_ms=7\n"
                "frame_00_capture_started_ms=3\n"
                "frame_00_capture_completed_ms=4\n",
                expected_frames=1,
            )

    def test_rejects_missing_frame_timing(self):
        with self.assertRaisesRegex(ValueError, "frame_00_capture_completed_ms"):
            gui.parse_reversal_helper_output(
                "initial_event_elapsed_ms=2\n"
                "pre_reverse_capture_started_elapsed_ms=8\n"
                "pre_reverse_capture_completed_elapsed_ms=8.5\n"
                "reversal_event_route=cghidEventTap\n"
                "reverse_event_elapsed_ms=9\n"
                "frame_00_capture_started_ms=3\n",
                expected_frames=1,
            )

    def test_rejects_process_targeted_event_routing(self):
        with self.assertRaisesRegex(ValueError, "did not use the cghidEventTap route"):
            gui.parse_reversal_helper_output(
                "initial_event_elapsed_ms=2\n"
                "pre_reverse_capture_started_elapsed_ms=8\n"
                "pre_reverse_capture_completed_elapsed_ms=8.5\n"
                "reversal_event_route=target_pid\n"
                "reverse_event_elapsed_ms=9\n"
                "frame_00_capture_started_ms=3\n"
                "frame_00_capture_completed_ms=4\n",
                expected_frames=1,
            )

    def test_reversal_helper_keeps_both_events_on_global_route(self):
        source = SWIFT_HELPER_PATH.read_text(encoding="utf-8")
        reversal = source.split("func wheelReversal(", 1)[1].split("\n}", 1)[0]
        self.assertEqual(reversal.count("postScroll(pid, unit,"), 2)
        self.assertIn("event.post(tap: .cghidEventTap)", source)
        self.assertNotIn("postToPid", source)
        self.assertIn('print("reversal_event_route=cghidEventTap")', reversal)
        self.assertIn("visibleLineNumbers(frame.image)", reversal)
        self.assertIn("preLines.min().map({ $0 > baseline }) == true", reversal)
        self.assertIn("guard confirmedOldDirection, let confirmedPreImage = preImage else", reversal)
        self.assertLess(
            reversal.index("guard confirmedOldDirection"),
            reversal.index("postScroll(pid, unit, reverseDelta)"),
        )
        self.assertIn("firstPosted + 0.135", reversal)

    def test_ocr_is_warmed_before_timed_reversal_and_capture_uses_display_metadata(self):
        source = SWIFT_HELPER_PATH.read_text(encoding="utf-8")
        prepare = source.split("func prepareWindowCaptureContext(", 1)[1].split("\n}", 1)[0]
        self.assertIn("visibleLineNumbers(warmFrame.image)", prepare)
        self.assertIn("SCScreenshotManager.captureImage", source)
        self.assertNotIn("captureSampleBuffer", source)
        self.assertIn("mach_absolute_time()", source)

    def test_reversal_capture_does_not_send_input_without_baseline(self):
        class NoHelperInteraction:
            def run_helper(self, *_args):
                raise AssertionError("helper must not run without a readable baseline")

        frames, pre_frame, error = gui.capture_frames(
            NoHelperInteraction(), None, None, None, "helper", 10, "window",
            Path("/tmp/hane-missing-scroll-baseline-test"), "lines", -8, (0, 24), 1.0,
            reverse_delta=12, baseline=None,
        )

        self.assertEqual(frames, [])
        self.assertIsNone(pre_frame)
        self.assertIn("基準可視行", error)

    def test_normal_capture_parser_uses_same_process_timing_and_global_route(self):
        evidence = gui.parse_scroll_capture_helper_output(
            "event_route=cghidEventTap\n"
            "event_post_elapsed_ms=1.250\n"
            "frame_00_capture_started_ms=0.500\n"
            "frame_00_capture_completed_ms=4.500\n"
            "frame_01_capture_started_ms=24.100\n"
            "frame_01_capture_completed_ms=27.100\n",
            expected_frames=2,
        )
        self.assertEqual(evidence["event_route"], "cghidEventTap")
        self.assertEqual(evidence["frame_elapsed_ms"], [4.5, 27.1])
        self.assertEqual(evidence["frame_capture_started_ms"], [0.5, 24.1])
        self.assertEqual(evidence["frame_capture_completed_ms"], [4.5, 27.1])

    def test_normal_frame_capture_posts_and_captures_inside_one_helper(self):
        source = SWIFT_HELPER_PATH.read_text(encoding="utf-8")
        normal_capture = source.split("func wheelCapture(", 1)[1].split("\n}", 1)[0]
        self.assertEqual(normal_capture.count("postScroll(pid, unit, delta)"), 1)
        self.assertIn("captureImageWithTimes(capture)", normal_capture)
        self.assertIn('print("event_route=cghidEventTap")', normal_capture)


class PixelsDirectFollowTests(unittest.TestCase):
    def test_accepts_immediate_motion_without_later_coast(self):
        result = gui.evaluate_pixels(100, frames([104, 104, 104], [8, 50, 200]))
        self.assertEqual(result["result"], "pass")
        self.assertTrue(result["immediate"])
        self.assertTrue(result["stable_without_app_coast"])

    def test_accepts_direct_follow_when_first_capture_precedes_screen_update(self):
        result = gui.evaluate_pixels(100, frames([100, 104, 104], [0, 50, 200]))
        self.assertEqual(result["result"], "pass")
        self.assertTrue(result["immediate"])
        self.assertEqual(result["first_response_frame"], 1)
        self.assertTrue(result["stable_without_app_coast"])

    def test_requires_screenshot_to_complete_inside_80ms_response_threshold(self):
        within_deadline_capture = frames([104, 104, 104], [77, 119, 199])
        result = gui.evaluate_pixels(100, within_deadline_capture)
        self.assertEqual(result["result"], "pass")
        self.assertTrue(result["immediate"])

        straddling_capture = frames([104, 104, 104], [80.1, 119, 199])
        result = gui.evaluate_pixels(100, straddling_capture)
        self.assertEqual(result["result"], "fail")
        self.assertFalse(result["immediate"])

    def test_samples_near_the_initial_response_deadline(self):
        self.assertIn(40, gui.FRAME_DELAYS_MS)
        self.assertIn(40, gui.PIXELS_FRAME_DELAYS_MS)

    def test_rejects_app_side_coast_after_pixels_event(self):
        result = gui.evaluate_pixels(100, frames([104, 106, 109], [8, 50, 200]))
        self.assertEqual(result["result"], "fail")
        self.assertFalse(result["stable_without_app_coast"])


class DocumentEdgeTests(unittest.TestCase):
    def test_accepts_reaching_and_staying_at_each_edge(self):
        top = gui.evaluate_document_edge(
            [
                {"visible_lines": [20, 21], "elapsed_ms": 10},
                {"visible_lines": [1, 2, 3], "elapsed_ms": 50},
                {"visible_lines": [1, 2, 3], "elapsed_ms": 100},
                {"visible_lines": [1, 2, 3], "elapsed_ms": 210},
            ],
            "top",
        )
        bottom = gui.evaluate_document_edge(
            [
                {"visible_lines": [470, 471], "elapsed_ms": 10},
                {"visible_lines": [498, 499, 500], "elapsed_ms": 50},
                {"visible_lines": [498, 499, 500], "elapsed_ms": 100},
                {"visible_lines": [498, 499, 500], "elapsed_ms": 210},
            ],
            "bottom",
        )
        self.assertEqual(top["result"], "pass")
        self.assertEqual(bottom["result"], "pass")

    def test_rejects_losing_the_edge_after_reaching_it(self):
        result = gui.evaluate_document_edge(
            [
                {"visible_lines": [20, 21], "elapsed_ms": 10},
                {"visible_lines": [1, 2, 3], "elapsed_ms": 50},
                {"visible_lines": [2, 3, 4], "elapsed_ms": 100},
                {"visible_lines": [3, 4, 5], "elapsed_ms": 210},
            ],
            "top",
        )
        self.assertEqual(result["result"], "fail")
        self.assertFalse(result["stayed_at_edge"])

    def test_blocks_when_ocr_does_not_observe_visible_lines(self):
        result = gui.evaluate_document_edge(
            [{"visible_lines": [1], "elapsed_ms": 10},
             {"visible_lines": [], "elapsed_ms": 50},
             {"visible_lines": [1], "elapsed_ms": 100},
             {"visible_lines": [1], "elapsed_ms": 210}],
            "top",
        )
        self.assertEqual(result["result"], "blocked")


if __name__ == "__main__":
    unittest.main()
