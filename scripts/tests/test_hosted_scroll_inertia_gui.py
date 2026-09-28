#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest

MODULE_PATH = Path(__file__).resolve().parents[1] / "hosted_scroll_inertia_gui.py"
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
                                        initial_to_reverse_event_ms=90)
        self.assertEqual(result["result"], "pass")
        self.assertTrue(result["old_direction_started"])
        self.assertTrue(result["prompt"])
        self.assertTrue(result["reversed_direction"])
        self.assertTrue(result["no_old_coast"])

    def test_rejects_old_direction_after_reversal(self):
        result = gui.evaluate_reversal(100, 108, frames([110, 109, 106, 104, 103, 103],
                                        [5, 24, 48, 80, 120, 180]),
                                        initial_to_reverse_event_ms=90)
        self.assertEqual(result["result"], "fail")
        self.assertFalse(result["prompt"])
        self.assertFalse(result["no_old_coast"])

    def test_blocks_when_capture_delays_reversal_past_inertia_window(self):
        result = gui.evaluate_reversal(
            100, 108, frames([107, 105, 102, 100, 99, 99], [5, 24, 48, 80, 120, 180]),
            initial_to_reverse_event_ms=136,
        )
        self.assertEqual(result["result"], "blocked")
        self.assertEqual(result["inertia_window_ms"], gui.LINES_INERTIA_WINDOW_MS)

    def test_blocks_when_reversal_timing_is_unavailable(self):
        result = gui.evaluate_reversal(
            100, 108, frames([107, 105, 102, 100, 99, 99], [5, 24, 48, 80, 120, 180]),
            initial_to_reverse_event_ms=None,
        )
        self.assertEqual(result["result"], "blocked")


class ReversalHelperTimingTests(unittest.TestCase):
    def test_parses_event_interval_and_frame_times(self):
        evidence = gui.parse_reversal_helper_output(
            "initial_event_elapsed_ms=2.000\n"
            "pre_reverse_capture_elapsed_ms=37.500\n"
            "reverse_event_elapsed_ms=40.250\n"
            "frame_00_elapsed_ms=4.100\n"
            "frame_01_elapsed_ms=25.000\n",
            expected_frames=2,
        )
        self.assertEqual(evidence["initial_to_reverse_event_ms"], 38.25)
        self.assertEqual(evidence["pre_reverse_capture_after_initial_ms"], 35.5)
        self.assertEqual(evidence["frame_elapsed_ms"], [4.1, 25.0])

    def test_rejects_out_of_order_pre_reversal_capture(self):
        with self.assertRaisesRegex(ValueError, "out of order"):
            gui.parse_reversal_helper_output(
                "initial_event_elapsed_ms=2\n"
                "pre_reverse_capture_elapsed_ms=8\n"
                "reverse_event_elapsed_ms=7\n"
                "frame_00_elapsed_ms=3\n",
                expected_frames=1,
            )

    def test_rejects_missing_frame_timing(self):
        with self.assertRaisesRegex(ValueError, "frame_00_elapsed_ms"):
            gui.parse_reversal_helper_output(
                "initial_event_elapsed_ms=2\n"
                "pre_reverse_capture_elapsed_ms=8\n"
                "reverse_event_elapsed_ms=9\n",
                expected_frames=1,
            )


class PixelsDirectFollowTests(unittest.TestCase):
    def test_accepts_immediate_motion_without_later_coast(self):
        result = gui.evaluate_pixels(100, frames([104, 104, 104], [8, 50, 200]))
        self.assertEqual(result["result"], "pass")
        self.assertTrue(result["immediate"])
        self.assertTrue(result["stable_without_app_coast"])

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
