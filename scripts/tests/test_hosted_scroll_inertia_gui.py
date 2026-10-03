#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

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

    def test_accepts_first_response_by_the_end_of_the_issue_inertia_window(self):
        result = gui.evaluate_lines_coast(
            100,
            frames([100, 100, 104, 106, 110, 112, 112, 112],
                   [44, 74, 120, 144, 170, 205, 240, 292]),
        )
        self.assertEqual(result["result"], "pass")
        self.assertTrue(result["first_response"])
        self.assertEqual(result["first_response_frame"], 2)

    def test_rejects_missing_initial_response(self):
        result = gui.evaluate_lines_coast(
            100,
            frames([100, 101, 102, 103, 104, 104, 104, 104],
                   [136, 160, 184, 208, 232, 256, 290, 340]),
        )
        self.assertEqual(result["result"], "fail")
        self.assertFalse(result["first_response"])

    def test_accepts_slow_but_in_window_response_using_post_response_rates(self):
        result = gui.evaluate_lines_coast(
            100,
            frames([100, 100, 100, 100, 100, 106, 109, 110, 110],
                   [0, 24, 40, 64, 108, 120, 144, 190, 240]),
        )
        self.assertEqual(result["result"], "pass")
        self.assertTrue(result["first_response"])
        self.assertEqual(result["first_response_frame"], 5)
        self.assertTrue(result["decelerated"])
        self.assertGreater(result["early_lines_per_ms"], result["late_lines_per_ms"])

    def test_blocks_slow_response_with_too_few_frames_to_judge_deceleration(self):
        result = gui.evaluate_lines_coast(
            100,
            frames([100, 100, 100, 100, 100, 106, 108],
                   [0, 24, 40, 64, 108, 120, 144]),
        )
        self.assertEqual(result["result"], "blocked")
        self.assertEqual(result["first_response_frame"], 5)

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


class ReversalProbeScheduleTests(unittest.TestCase):
    def test_default_schedule_leaves_time_to_finish_the_last_screen_capture(self):
        delays = gui.PRE_REVERSE_PROBE_DELAYS_MS
        self.assertEqual(len(delays), 2)
        self.assertEqual(delays, tuple(sorted(set(delays))))
        # The third sequential capture exceeded the 135ms limit on a local
        # Mac. Two probes starting at 32ms and 72ms leave time for capture
        # completion before reversal, even when each screenshot takes ~30ms.
        self.assertEqual(delays, (32, 72))
        self.assertLessEqual(delays[-1], 100)
        self.assertLess(delays[-1], gui.LINES_INERTIA_WINDOW_MS)


class ReversalHelperTimingTests(unittest.TestCase):
    def test_parses_event_interval_and_pre_and_post_frame_times(self):
        evidence = gui.parse_reversal_helper_output(
            "initial_event_elapsed_ms=2.000\n"
            "pre_frame_00_capture_started_ms=10.000\n"
            "pre_frame_00_capture_completed_ms=11.000\n"
            "pre_frame_01_capture_started_ms=36.500\n"
            "pre_frame_01_capture_completed_ms=37.500\n"
            "reversal_event_route=cghidEventTap\n"
            "reverse_event_elapsed_ms=40.250\n"
            "frame_00_capture_started_ms=4.100\n"
            "frame_00_capture_completed_ms=5.100\n"
            "frame_01_capture_started_ms=25.000\n"
            "frame_01_capture_completed_ms=26.000\n",
            expected_pre_frames=2,
            expected_frames=2,
        )
        self.assertEqual(evidence["event_route"], "cghidEventTap")
        self.assertEqual(evidence["initial_to_reverse_event_ms"], 38.25)
        self.assertEqual(evidence["pre_frame_capture_started_after_initial_ms"], [8.0, 34.5])
        self.assertEqual(evidence["pre_frame_capture_completed_after_initial_ms"], [9.0, 35.5])
        self.assertEqual(evidence["frame_elapsed_ms"], [5.1, 26.0])
        self.assertEqual(evidence["frame_capture_started_ms"], [4.1, 25.0])
        self.assertEqual(evidence["frame_capture_completed_ms"], [5.1, 26.0])

    def test_rejects_pre_reversal_capture_completed_after_the_reverse_event(self):
        with self.assertRaisesRegex(ValueError, "out of order"):
            gui.parse_reversal_helper_output(
                "initial_event_elapsed_ms=2\n"
                "pre_frame_00_capture_started_ms=8\n"
                "pre_frame_00_capture_completed_ms=9\n"
                "reversal_event_route=cghidEventTap\n"
                "reverse_event_elapsed_ms=7\n"
                "frame_00_capture_started_ms=3\n"
                "frame_00_capture_completed_ms=4\n",
                expected_pre_frames=1,
                expected_frames=1,
            )

    def test_rejects_missing_frame_timing(self):
        with self.assertRaisesRegex(ValueError, "frame_00_capture_completed_ms"):
            gui.parse_reversal_helper_output(
                "initial_event_elapsed_ms=2\n"
                "pre_frame_00_capture_started_ms=8\n"
                "pre_frame_00_capture_completed_ms=8.5\n"
                "reversal_event_route=cghidEventTap\n"
                "reverse_event_elapsed_ms=9\n"
                "frame_00_capture_started_ms=3\n",
                expected_pre_frames=1,
                expected_frames=1,
            )

    def test_rejects_process_targeted_event_routing(self):
        with self.assertRaisesRegex(ValueError, "did not use the cghidEventTap route"):
            gui.parse_reversal_helper_output(
                "initial_event_elapsed_ms=2\n"
                "pre_frame_00_capture_started_ms=8\n"
                "pre_frame_00_capture_completed_ms=8.5\n"
                "reversal_event_route=target_pid\n"
                "reverse_event_elapsed_ms=9\n"
                "frame_00_capture_started_ms=3\n"
                "frame_00_capture_completed_ms=4\n",
                expected_pre_frames=1,
                expected_frames=1,
            )

    def test_reversal_helper_captures_every_pre_reverse_candidate_before_sending_reverse_input(self):
        source = SWIFT_HELPER_PATH.read_text(encoding="utf-8")
        reversal = source.split("func wheelReversal(", 1)[1].split("\n}", 1)[0]
        self.assertEqual(reversal.count("postScroll(pid, unit,"), 2)
        self.assertLess(
            reversal.index("for (index, delayMs) in preProbeDelaysMs.enumerated()"),
            reversal.index("let reversePosted = postScroll(pid, unit, reverseDelta)"),
            "every pre-reversal candidate frame must be captured before the reverse input is sent",
        )
        self.assertNotIn(
            "visibleLineNumbers", reversal,
            "OCR must not run inside the reversal helper's timed window",
        )
        self.assertIn("event.post(tap: .cghidEventTap)", source)
        self.assertNotIn("postToPid", source)
        self.assertIn('print("reversal_event_route=cghidEventTap")', reversal)
        self.assertIn("frame.completed < firstPosted + 0.135", reversal)
        self.assertIn("let startedMs = milliseconds(frame.started - firstPosted)", reversal)
        self.assertIn("let completedMs = milliseconds(frame.completed - firstPosted)", reversal)
        self.assertIn("the Lines inertia deadline is 135ms", reversal)

    def test_wheel_reversal_cli_bounds_pre_reverse_probe_delays_to_the_inertia_window(self):
        source = SWIFT_HELPER_PATH.read_text(encoding="utf-8")
        self.assertIn("preProbeDelays.allSatisfy({ $0 > 0 && $0 <= 130 })", source)

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


class OldDirectionCandidateSelectionTests(unittest.TestCase):
    def test_returns_none_when_no_candidate_moved_past_baseline(self):
        candidates = [{"visible_lines": [100, 101]}, {"visible_lines": [100, 101]}]
        self.assertIsNone(gui.select_old_direction_candidate(100, candidates))

    def test_returns_none_when_ocr_found_no_lines(self):
        candidates = [{"visible_lines": []}, {"visible_lines": None}]
        self.assertIsNone(gui.select_old_direction_candidate(100, candidates))

    def test_prefers_the_latest_candidate_that_captured_old_direction_motion(self):
        candidates = [
            {"visible_lines": [100, 101], "capture_completed_after_initial_ms": 60},
            {"visible_lines": [104, 105], "capture_completed_after_initial_ms": 96},
            {"visible_lines": [108, 109], "capture_completed_after_initial_ms": 118},
        ]
        selected = gui.select_old_direction_candidate(100, candidates)
        self.assertEqual(selected["capture_completed_after_initial_ms"], 118)
        self.assertEqual(selected["value"], 108)


class ReversalCaptureFramesTests(unittest.TestCase):
    class _StubInteraction:
        def __init__(self, reversal_output, ocr_texts):
            self.reversal_output = reversal_output
            self.ocr_texts = ocr_texts

        def run_helper(self, _helper, args, _timeout):
            if args[0] == "wheel-reversal":
                return True, self.reversal_output, ""
            if args[0] == "ocr":
                return True, self.ocr_texts.get(Path(args[1]).name, ""), ""
            raise AssertionError(f"unexpected helper command: {args[0]}")

    def test_selects_the_candidate_that_captured_old_direction_motion(self):
        # Two candidates are captured within the window without OCR; the
        # second one happened to land on the moved frame. OCR runs after the
        # helper call returns, once the reverse input has already been sent.
        reversal_output = (
            "initial_event_elapsed_ms=2\n"
            "reversal_event_route=cghidEventTap\n"
            "reverse_event_elapsed_ms=102\n"
            "pre_frame_00_capture_started_ms=32\n"
            "pre_frame_00_capture_completed_ms=34\n"
            "pre_frame_01_capture_started_ms=72\n"
            "pre_frame_01_capture_completed_ms=74\n"
            "frame_00_capture_started_ms=5\n"
            "frame_00_capture_completed_ms=6\n"
        )
        ocr_texts = {
            "pre-frame-00.png": "LINE 100",
            "pre-frame-01.png": "LINE 108",
        }
        interaction = self._StubInteraction(reversal_output, ocr_texts)
        frames, pre_reverse, error = gui.capture_frames(
            interaction, None, None, None, "helper", 10, "window",
            Path("/tmp/hane-reversal-candidate-selected-test"), "lines", 8, (0,), 1.0,
            reverse_delta=-12, baseline=100,
            pre_reverse_probe_delays_ms=gui.PRE_REVERSE_PROBE_DELAYS_MS,
        )
        self.assertIsNone(error)
        self.assertEqual(len(pre_reverse["candidates"]), 2)
        self.assertTrue(pre_reverse["selected"]["path"].endswith("pre-frame-01.png"))
        self.assertEqual(pre_reverse["selected"]["value"], 108)

    def test_reports_no_selection_when_no_candidate_shows_old_direction(self):
        # None of the candidate images happened to capture the old-direction
        # motion; this must surface as "no selection" (observation shortfall)
        # rather than a synthesized pre-reverse value.
        reversal_output = (
            "initial_event_elapsed_ms=2\n"
            "reversal_event_route=cghidEventTap\n"
            "reverse_event_elapsed_ms=130\n"
            "pre_frame_00_capture_started_ms=66\n"
            "pre_frame_00_capture_completed_ms=68\n"
            "frame_00_capture_started_ms=5\n"
            "frame_00_capture_completed_ms=6\n"
        )
        ocr_texts = {"pre-frame-00.png": "LINE 100"}
        interaction = self._StubInteraction(reversal_output, ocr_texts)
        frames, pre_reverse, error = gui.capture_frames(
            interaction, None, None, None, "helper", 10, "window",
            Path("/tmp/hane-reversal-no-candidate-test"), "lines", -8, (0,), 1.0,
            reverse_delta=12, baseline=100, pre_reverse_probe_delays_ms=(64,),
        )
        self.assertIsNone(error)
        self.assertEqual(len(pre_reverse["candidates"]), 1)
        self.assertIsNone(pre_reverse["selected"])


class FrameScheduleTests(unittest.TestCase):
    def test_lines_and_pixels_schedules_add_a_completable_point_before_the_next_delay(self):
        # 108ms -> 144ms (Lines) and 112ms -> 160ms (Pixels) both jump past
        # the 135ms deadline, and existing delays must stay unchanged.
        self.assertIn(120, gui.FRAME_DELAYS_MS)
        self.assertIn(128, gui.PIXELS_FRAME_DELAYS_MS)
        self.assertTrue({0, 24, 40, 64, 108, 144, 190, 240}.issubset(set(gui.FRAME_DELAYS_MS)))
        self.assertTrue({0, 24, 40, 64, 88, 112, 160, 200}.issubset(set(gui.PIXELS_FRAME_DELAYS_MS)))

    def test_lines_coast_detects_a_response_only_visible_at_the_added_120ms_capture(self):
        # Measured (actual) capture-completion times: the 108ms point
        # completes at 110ms with no response yet, and without the fix the
        # next point would be 144ms -- already past the 135ms deadline. The
        # added 120ms point's real completion (122ms) must catch the response.
        result = gui.evaluate_lines_coast(
            100,
            frames([100, 100, 100, 100, 100, 106, 110, 112, 112],
                   [8, 24, 48, 72, 110, 122, 146, 190, 240]),
        )
        self.assertTrue(result["first_response"])
        self.assertEqual(result["first_response_frame"], 5)

    def test_lines_coast_evaluates_deceleration_when_response_lands_exactly_on_the_split_frame(self):
        # Scheduled frame 4 (108ms) actually completes capture at 120ms and
        # is the first frame to show a response, colliding with the fixed
        # split_index=4. The remaining frames must still be split in half to
        # judge deceleration instead of comparing the response frame to
        # itself (which previously forced early_lines_per_ms to 0).
        result = gui.evaluate_lines_coast(
            100,
            frames([100, 100, 100, 100, 104, 106, 108, 109, 109],
                   [8, 24, 48, 72, 120, 146, 190, 230, 240]),
        )
        self.assertEqual(result["result"], "pass")
        self.assertTrue(result["first_response"])
        self.assertEqual(result["first_response_frame"], 4)
        self.assertTrue(result["decelerated"])
        self.assertGreater(result["early_lines_per_ms"], 0)
        self.assertGreater(result["early_lines_per_ms"], result["late_lines_per_ms"])

    def test_pixels_detects_a_response_only_visible_at_the_added_128ms_capture(self):
        result = gui.evaluate_pixels(
            100,
            frames([100, 100, 100, 100, 100, 104, 104],
                   [8, 24, 48, 64, 90, 130, 200]),
        )
        self.assertTrue(result["immediate"])
        self.assertEqual(result["first_response_frame"], 5)


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

    def test_requires_screenshot_to_complete_inside_lines_inertia_window(self):
        within_deadline_capture = frames([104, 104, 104], [134.9, 159, 199])
        result = gui.evaluate_pixels(100, within_deadline_capture)
        self.assertEqual(result["result"], "pass")
        self.assertTrue(result["immediate"])

        straddling_capture = frames([104, 104, 104], [135.1, 159, 199])
        result = gui.evaluate_pixels(100, straddling_capture)
        self.assertEqual(result["result"], "fail")
        self.assertFalse(result["immediate"])

    def test_samples_near_the_initial_response_deadline(self):
        self.assertIn(40, gui.FRAME_DELAYS_MS)
        self.assertTrue({64, 88, 112}.issubset(gui.PIXELS_FRAME_DELAYS_MS))

    def test_pixels_samples_cover_the_response_window_and_later_stability(self):
        self.assertTrue(any(delay >= 112 for delay in gui.PIXELS_FRAME_DELAYS_MS))
        self.assertGreaterEqual(gui.PIXELS_FRAME_DELAYS_MS[-1], 180)

    def test_rejects_app_side_coast_after_pixels_event(self):
        result = gui.evaluate_pixels(100, frames([104, 106, 109], [8, 50, 200]))
        self.assertEqual(result["result"], "fail")
        self.assertFalse(result["stable_without_app_coast"])


class ScrollEventObservationAttachmentTests(unittest.TestCase):
    def test_attaching_a_disordered_observation_does_not_change_a_failing_producer_result(self):
        step = gui.evaluate_lines_coast(
            100,
            frames([100, 101, 102, 103, 104, 104, 104, 104],
                   [136, 160, 184, 208, 232, 256, 290, 340]),
        )
        self.assertEqual(step["result"], "fail")
        observation = {"observation": "observed_disordered", "reason": "順序不整合",
                       "stages_ms": {}, "clock_consistent": True,
                       "presentation_observation": "unavailable"}
        attached = gui.attach_scroll_event_observation(step, observation)
        self.assertEqual(attached["result"], "fail")
        self.assertFalse(attached["first_response"])
        self.assertEqual(attached["scroll_event_observation"], observation)

    def test_attaching_an_unavailable_observation_does_not_change_a_passing_producer_result(self):
        step = gui.evaluate_lines_coast(
            100,
            frames([104, 106, 109, 111, 112, 113, 113, 113],
                   [8, 24, 48, 72, 108, 144, 190, 240]),
        )
        self.assertEqual(step["result"], "pass")
        attached = gui.attach_scroll_event_observation(step, None)
        self.assertEqual(attached["result"], "pass")
        self.assertIsNone(attached["scroll_event_observation"])

    def test_attaching_an_observation_does_not_mutate_the_original_step(self):
        step = gui.evaluate_lines_coast(
            100,
            frames([104, 106, 109, 111, 112, 113, 113, 113],
                   [8, 24, 48, 72, 108, 144, 190, 240]),
        )
        gui.attach_scroll_event_observation(step, {"observation": "observed_ordered"})
        self.assertNotIn("scroll_event_observation", step)

    # Regression fixtures (Issue #427): the raw producer judgment
    # (pass/fail/blocked from evaluate_lines_coast/evaluate_pixels) and the
    # separate observer classification must stay independent in every
    # combination below -- a disordered or unavailable observation can never
    # upgrade a fail/blocked producer result to pass, and an ordered
    # observation never downgrades or hides a fail/blocked one either.

    def test_normal_ordered_observation_does_not_change_a_passing_producer_result(self):
        step = gui.evaluate_lines_coast(
            100,
            frames([104, 106, 109, 111, 112, 113, 113, 113],
                   [8, 24, 48, 72, 108, 144, 190, 240]),
        )
        self.assertEqual(step["result"], "pass")
        observation = {"observation": "observed_ordered", "reason": None,
                       "stages_ms": {}, "clock_consistent": True,
                       "presentation_observation": "unavailable"}
        attached = gui.attach_scroll_event_observation(step, observation)
        self.assertEqual(attached["result"], "pass")
        self.assertEqual(attached["scroll_event_observation"], observation)

    def test_attaching_an_unavailable_observation_does_not_change_a_failing_producer_result(self):
        step = gui.evaluate_lines_coast(
            100,
            frames([100, 100, 100, 100, 100, 100, 100, 100],
                   [8, 24, 48, 72, 108, 144, 190, 240]),
        )
        self.assertEqual(step["result"], "fail")
        observation = {"observation": "unavailable", "reason": "計測不能",
                       "stages_ms": {}, "clock_consistent": False,
                       "presentation_observation": "unavailable"}
        attached = gui.attach_scroll_event_observation(step, observation)
        self.assertEqual(attached["result"], "fail")
        self.assertEqual(attached["scroll_event_observation"], observation)

    def test_attaching_an_observation_does_not_change_a_blocked_producer_result(self):
        step = gui.evaluate_lines_coast(100, frames([100], [8]))
        self.assertEqual(step["result"], "blocked")
        for observation in (
            None,
            {"observation": "unavailable", "reason": "計測不能", "stages_ms": {},
             "clock_consistent": False, "presentation_observation": "unavailable"},
            {"observation": "observed_ordered", "reason": None, "stages_ms": {},
             "clock_consistent": True, "presentation_observation": "unavailable"},
        ):
            with self.subTest(observation=observation):
                attached = gui.attach_scroll_event_observation(step, observation)
                self.assertEqual(attached["result"], "blocked")
                self.assertEqual(attached["scroll_event_observation"], observation)


class CaptureFramesScrollEventMeasurementTests(unittest.TestCase):
    class _StubInteraction:
        def __init__(self, output, ocr_text="LINE 100"):
            self.output = output
            self.ocr_text = ocr_text
            self.calls: list[list[str]] = []

        def run_helper(self, _helper, args, _timeout):
            self.calls.append(args)
            if args[0] == "wheel-measure":
                return True, self.output, ""
            if args[0] == "ocr":
                return True, self.ocr_text, ""
            raise AssertionError(f"unexpected helper command: {args[0]}")

    class _StubObservationModule:
        def __init__(self):
            self.parse_calls: list[tuple[str, int]] = []
            self.assessed = {"observation": "observed_ordered", "reason": None,
                             "stages_ms": {}, "clock_consistent": True,
                             "presentation_observation": "unavailable"}

        def parse_wheel_measure_output(self, output, expected_frames):
            self.parse_calls.append((output, expected_frames))
            return {"stub_record": True}

        def assess_wheel_measurement(self, record):
            assert record == {"stub_record": True}
            return self.assessed

    WHEEL_MEASURE_OUTPUT = (
        "event_route=cghidEventTap\n"
        "event_post_ticks=1000000\n"
        "mach_timebase_numer=1\n"
        "mach_timebase_denom=1\n"
        "product_scroll_receipt_ticks=1100000\n"
        "product_frame_paint_ticks=1400000\n"
        "product_frame_presented_ticks=unavailable\n"
        "product_mach_timebase_numer=1\n"
        "product_mach_timebase_denom=1\n"
        "frame_00_capture_started_ticks=2000000\n"
        "frame_00_capture_completed_ticks=2200000\n"
        "display_response_count=2\n"
        "display_response_collection_valid=true\n"
        "display_response_00_sample_id=7\n"
        "display_response_00_frame_status=complete\n"
        "display_response_00_timestamp_source=SCStreamFrameInfo.displayTime\n"
        "display_response_00_image_source=same_CMSampleBuffer\n"
        "display_response_00_display_time_ticks=1800000\n"
        "display_response_00_callback_received_ticks=3000000\n"
        "display_response_00_image_ready_ticks=3100000\n"
        "display_response_00_artifact_written_ticks=3400000\n"
        "display_response_00_image_path=/tmp/hane-wheel-measure-wiring-test/frames/display-response-00.png\n"
        "display_response_01_sample_id=8\n"
        "display_response_01_frame_status=complete\n"
        "display_response_01_timestamp_source=SCStreamFrameInfo.displayTime\n"
        "display_response_01_image_source=same_CMSampleBuffer\n"
        "display_response_01_display_time_ticks=1900000\n"
        "display_response_01_callback_received_ticks=3100000\n"
        "display_response_01_image_ready_ticks=3200000\n"
        "display_response_01_artifact_written_ticks=3400000\n"
        "display_response_01_image_path=/tmp/hane-wheel-measure-wiring-test/frames/display-response-01.png\n"
    )

    def test_uses_wheel_measure_and_attaches_a_separate_observer_classification(self):
        interaction = self._StubInteraction(self.WHEEL_MEASURE_OUTPUT)
        observation_module = self._StubObservationModule()
        timing_path = Path("/tmp/hane-wheel-measure-wiring-test/timing.log")
        frames_out, observation, error = gui.capture_frames(
            interaction, None, None, None, "helper", 10, "window",
            Path("/tmp/hane-wheel-measure-wiring-test"), "lines", -8, (0,), 1.0,
            scroll_event_timing_path=timing_path,
            scroll_event_observation_module=observation_module,
        )
        self.assertIsNone(error)
        self.assertEqual(interaction.calls[0][0], "wheel-measure")
        self.assertIn(str(timing_path), interaction.calls[0])
        self.assertEqual(len(frames_out), 1)
        self.assertAlmostEqual(frames_out[0]["capture_started_elapsed_ms"], 1.0)
        self.assertAlmostEqual(frames_out[0]["capture_completed_elapsed_ms"], 1.2)
        self.assertEqual(observation_module.parse_calls, [(self.WHEEL_MEASURE_OUTPUT, 1)])
        self.assertEqual(observation, observation_module.assessed)
        self.assertEqual(observation["window_server_display_responses"][0]["visible_lines"], [100])
        self.assertEqual(observation["window_server_display_responses"][0]["sample_id"], 7)
        self.assertEqual(len(observation["window_server_display_responses"]), 2)
        self.assertEqual([call[0] for call in interaction.calls].count("ocr"), 3)

    def test_measurement_parse_failure_preserves_stdout_for_diagnosis(self):
        output = self.WHEEL_MEASURE_OUTPUT.replace(
            "event_route=cghidEventTap", "event_route=target_pid")
        output += "\nextra=" + ("x" * (gui.WHEEL_MEASURE_ERROR_OUTPUT_LIMIT + 100))
        interaction = self._StubInteraction(output)
        observation_module = self._StubObservationModule()
        frames_out, observation, error = gui.capture_frames(
            interaction, None, None, None, "helper", 10, "window",
            Path("/tmp/hane-wheel-measure-invalid-output-test"), "lines", -120, (0,), 1.0,
            scroll_event_timing_path=Path("/tmp/hane-wheel-measure-invalid-output-test/timing.log"),
            scroll_event_observation_module=observation_module,
        )
        self.assertEqual(frames_out, [])
        self.assertIsNone(observation)
        self.assertIn("did not use the cghidEventTap route", error)
        self.assertIn("wheel-measure output:", error)
        self.assertIn("product_scroll_receipt_ticks=1100000", error)
        diagnostic_output = error.split("wheel-measure output:\n", 1)[1]
        self.assertEqual(
            diagnostic_output,
            output[:gui.WHEEL_MEASURE_ERROR_OUTPUT_LIMIT] + "\n...(truncated)",
        )
        self.assertEqual(observation_module.parse_calls, [])

    def test_window_server_display_time_is_separate_from_late_callback(self):
        output = self.WHEEL_MEASURE_OUTPUT.replace(
            "display_response_00_display_time_ticks=1800000",
            "display_response_00_display_time_ticks=81000000",
        ).replace(
            "display_response_00_callback_received_ticks=3000000",
            "display_response_00_callback_received_ticks=106000000",
        ).replace(
            "display_response_00_image_ready_ticks=3100000",
            "display_response_00_image_ready_ticks=107000000",
        ).replace(
            "display_response_00_artifact_written_ticks=3200000",
            "display_response_00_artifact_written_ticks=108000000",
        ).replace(
            "display_response_00_artifact_written_ticks=3400000",
            "display_response_00_artifact_written_ticks=108000000",
        ).replace(
            "display_response_01_display_time_ticks=1900000",
            "display_response_01_display_time_ticks=82000000",
        ).replace(
            "display_response_01_callback_received_ticks=3100000",
            "display_response_01_callback_received_ticks=107000000",
        ).replace(
            "display_response_01_image_ready_ticks=3200000",
            "display_response_01_image_ready_ticks=108000000",
        ).replace(
            "display_response_01_artifact_written_ticks=3400000",
            "display_response_01_artifact_written_ticks=109000000",
        )
        evidence = gui.parse_wheel_measure_capture_output(output, 1)
        self.assertAlmostEqual(evidence["display_responses"][0]["display_time_elapsed_ms"], 80.0)
        self.assertAlmostEqual(evidence["display_responses"][0]["callback_received_elapsed_ms"], 105.0)

    def test_missing_malformed_unpresented_or_unordered_display_metadata_fails_closed(self):
        mutations = (
            ("display_response_00_frame_status=complete", "display_response_00_frame_status=idle"),
            ("display_response_00_timestamp_source=SCStreamFrameInfo.displayTime", "display_response_00_timestamp_source=other"),
            ("display_response_00_image_source=same_CMSampleBuffer", "display_response_00_image_source=other_sample"),
            ("display_response_00_sample_id=7", "display_response_00_sample_id=0"),
            ("display_response_00_sample_id=7", "display_response_00_sample_id=-1"),
            ("display_response_00_display_time_ticks=1800000", "display_response_00_display_time_ticks=unavailable"),
            ("display_response_00_display_time_ticks=1800000", "display_response_00_display_time_ticks=18446744073709551616"),
            ("display_response_00_callback_received_ticks=3000000", "display_response_00_callback_received_ticks=500000"),
        )
        for old, new in mutations:
            with self.subTest(change=new):
                output = self.WHEEL_MEASURE_OUTPUT.replace(old, new)
                with self.assertRaises(ValueError):
                    gui.parse_wheel_measure_capture_output(output, 1)

        for old, new in (
            ("display_response_collection_valid=true", "display_response_collection_valid=false"),
            ("display_response_count=2", "display_response_count=3"),
        ):
            with self.subTest(change=new), self.assertRaises(ValueError):
                gui.parse_wheel_measure_capture_output(self.WHEEL_MEASURE_OUTPUT.replace(old, new), 1)

    def test_wheel_measure_helper_failure_is_not_turned_into_a_pass(self):
        # A Vision warm-up crash (or any other helper failure) surfaces
        # through wheel-measure the same as wheel-capture: no frames, no
        # observation, just the raw error for the caller to report as
        # blocked/measurement-unavailable -- never silently as a pass.
        def run_helper(_helper, args, _timeout):
            if args[0] == "wheel-measure":
                return False, "", "could not inspect captured scroll frame: Vision crashed"
            raise AssertionError(f"unexpected helper command: {args[0]}")

        interaction = self._StubInteraction("")
        interaction.run_helper = run_helper
        observation_module = self._StubObservationModule()
        frames_out, observation, error = gui.capture_frames(
            interaction, None, None, None, "helper", 10, "window",
            Path("/tmp/hane-wheel-measure-failure-test"), "lines", -8, (0,), 1.0,
            scroll_event_timing_path=Path("/tmp/hane-wheel-measure-failure-test/timing.log"),
            scroll_event_observation_module=observation_module,
        )
        self.assertEqual(frames_out, [])
        self.assertIsNone(observation)
        self.assertEqual(error, "could not inspect captured scroll frame: Vision crashed")
        self.assertEqual(observation_module.parse_calls, [])

    def test_without_scroll_event_timing_path_falls_back_to_wheel_capture_unchanged(self):
        def run_helper(_helper, args, _timeout):
            if args[0] == "wheel-capture":
                return True, (
                    "event_route=cghidEventTap\n"
                    "event_post_elapsed_ms=1.0\n"
                    "frame_00_capture_started_ms=2.0\n"
                    "frame_00_capture_completed_ms=3.0\n"
                ), ""
            if args[0] == "ocr":
                return True, "LINE 100", ""
            raise AssertionError(f"unexpected helper command: {args[0]}")

        interaction = self._StubInteraction("")
        interaction.run_helper = run_helper
        frames_out, observation, error = gui.capture_frames(
            interaction, None, None, None, "helper", 10, "window",
            Path("/tmp/hane-wheel-capture-default-test"), "lines", -8, (0,), 1.0,
        )
        self.assertIsNone(error)
        self.assertIsNone(observation)
        self.assertEqual(len(frames_out), 1)

    def test_missing_observation_module_also_falls_back_to_wheel_capture(self):
        # Opting into HANE_SCROLL_EVENT_TIMING_PATH alone must not switch the
        # helper command without an observation module to interpret it with.
        def run_helper(_helper, args, _timeout):
            if args[0] == "wheel-capture":
                return True, (
                    "event_route=cghidEventTap\n"
                    "event_post_elapsed_ms=1.0\n"
                    "frame_00_capture_started_ms=2.0\n"
                    "frame_00_capture_completed_ms=3.0\n"
                ), ""
            if args[0] == "ocr":
                return True, "LINE 100", ""
            raise AssertionError(f"unexpected helper command: {args[0]}")

        interaction = self._StubInteraction("")
        interaction.run_helper = run_helper
        frames_out, observation, error = gui.capture_frames(
            interaction, None, None, None, "helper", 10, "window",
            Path("/tmp/hane-wheel-capture-no-module-test"), "lines", -8, (0,), 1.0,
            scroll_event_timing_path=Path("/tmp/hane-wheel-capture-no-module-test/timing.log"),
        )
        self.assertIsNone(error)
        self.assertIsNone(observation)
        self.assertEqual(len(frames_out), 1)


class PositionDocumentMidpointMeasurementTests(unittest.TestCase):
    def _run_position(self, final_lines, capture_error=None):
        top_step = {"result": "pass"}
        top_lines = [1, 2, 3]
        top_text = "LINE 001\nLINE 002"
        position_frames = [
            {"visible_lines": [1, 2], "recognized_text": "LINE 001"},
            {"visible_lines": final_lines, "recognized_text": "position frame"},
        ]
        observation = {"observation": "observed_ordered", "receipt_ticks": 123}
        timing_path = Path("/tmp/hane-position-measurement-test/timing.log")
        observation_module = object()
        with patch.object(gui, "move_to_document_top", return_value=(top_step, top_lines, top_text)), \
                patch.object(gui, "capture_frames", return_value=(position_frames, observation, capture_error)) as capture:
            result, lines, text = gui.position_document_midpoint(
                object(), object(), object(), object(), "helper", 123, "window",
                Path("/tmp/hane-position-measurement-test"), 2.0, -1,
                "lines-positioning", timing_path, observation_module,
            )
        return result, lines, text, capture, position_frames, observation, timing_path, observation_module

    def test_positioning_uses_measured_120_line_input_and_preserves_six_frame_schedule(self):
        result, lines, text, capture, position_frames, observation, timing_path, observation_module = (
            self._run_position([180, 181, 182])
        )
        self.assertEqual(result["result"], "pass")
        self.assertEqual(lines, [180, 181, 182])
        self.assertEqual(text, "position frame")
        self.assertEqual(result["frames"], position_frames)
        self.assertIs(result["scroll_event_observation"], observation)
        args, kwargs = capture.call_args
        self.assertEqual(args[7], Path("/tmp/hane-position-measurement-test/lines-positioning-scroll"))
        self.assertEqual(args[8:12], ("lines", -120, (0, 48, 96, 144, 200, 240), 2.0))
        self.assertEqual(kwargs["scroll_event_timing_path"], timing_path)
        self.assertIs(kwargs["scroll_event_observation_module"], observation_module)

    def test_still_at_document_top_remains_blocked_and_keeps_measurement_evidence(self):
        result, lines, text, _capture, position_frames, observation, *_ = self._run_position([1, 2, 3])
        self.assertEqual(result["result"], "blocked")
        self.assertIn("文書中央付近", result["reason"])
        self.assertEqual(lines, [1, 2, 3])
        self.assertEqual(text, "position frame")
        self.assertEqual(result["frames"], position_frames)
        self.assertIs(result["scroll_event_observation"], observation)

    def test_capture_error_remains_blocked_and_keeps_partial_measurement_evidence(self):
        result, lines, text, _capture, position_frames, observation, *_ = self._run_position(
            [180, 181], "scroll receipt unavailable"
        )
        self.assertEqual(result["result"], "blocked")
        self.assertEqual(result["reason"], "scroll receipt unavailable")
        self.assertIsNone(lines)
        self.assertEqual(text, "")
        self.assertEqual(result["frames"], position_frames)
        self.assertIs(result["scroll_event_observation"], observation)


class PixelsScrollEventMeasurementWiringTests(unittest.TestCase):
    # The focused checker uses one calibrated direction for Lines and Pixels,
    # and both measured captures opt into the same wheel-measure path.
    def test_pixels_capture_opts_into_wheel_measure_with_the_shared_timing_path(self):
        source = MODULE_PATH.read_text(encoding="utf-8")
        body = source.split("def run_scroll_behavior_checks(", 1)[1]
        pixels_call = body.split('"pixels", downward_sign * 180, PIXELS_FRAME_DELAYS_MS, helper_timeout,', 1)[1]
        pixels_call = pixels_call.split(")", 1)[0]
        self.assertIn("scroll_event_timing_path=scroll_event_timing_path", pixels_call)
        self.assertIn("scroll_event_observation_module=scroll_event_observation_module", pixels_call)

    def test_pixels_capture_requires_a_valid_mid_document_baseline(self):
        source = MODULE_PATH.read_text(encoding="utf-8")
        body = source.split("def run_scroll_behavior_checks(", 1)[1]
        position = body.split('pixel_position, before_lines, before_text = position_document_midpoint(', 1)[1]
        branch = body.split('if pixel_position["result"] == "pass":', 1)[1]
        capture = branch.split('frames, pixels_scroll_event_observation, error = capture_frames(', 1)[0]
        self.assertIn('"pixels-before"', position)
        self.assertIn('step("pixels_direct_follow", "blocked", pixel_position.get("reason"))', branch)
        self.assertIn('"pixels", downward_sign * 180, PIXELS_FRAME_DELAYS_MS, helper_timeout,', branch)
        self.assertNotIn("capture_frames(", capture)

    def test_lines_positioning_uses_the_shared_timing_path_and_observer(self):
        source = MODULE_PATH.read_text(encoding="utf-8")
        body = source.split("def run_scroll_behavior_checks(", 1)[1]
        position_call = body.split('position, position_lines, _position_text = position_document_midpoint(', 1)[1]
        position_call = position_call.split(")", 1)[0]
        self.assertIn('"lines-positioning"', position_call)
        self.assertIn("scroll_event_timing_path, scroll_event_observation_module", position_call)


class ScrollDirectionCalibrationTests(unittest.TestCase):
    def test_calibration_probes_both_signs_from_the_verified_document_top(self):
        source = MODULE_PATH.read_text(encoding="utf-8")
        body = source.split("def calibrate_scroll_direction(", 1)[1].split("\ndef position_document_midpoint", 1)[0]
        self.assertIn('"direction-calibration-top"', body)
        self.assertIn('((1, "positive"), (-1, "negative"))', body)
        self.assertIn("offset > baseline", body)
        self.assertIn("direction-calibration-final-reset", body)

    def test_moves_caret_past_first_line_before_ocr(self):
        class RecordingInteraction:
            def __init__(self):
                self.calls = []

            def run_helper(self, helper, arguments, timeout):
                self.calls.append((helper, arguments, timeout))
                return True, "", None

        interaction = RecordingInteraction()
        expected_calls = [
            ("helper", ["move-doc-start", "123"], 1.0),
            ("helper", ["move-caret", "123", "right", "8"], 1.0),
        ]

        def capture_after_caret_move(*_args):
            self.assertEqual(interaction.calls, expected_calls)
            return {"result": "pass"}, [1, 2], "LINE 001\nLINE 002"

        with patch.object(gui, "capture_single", side_effect=capture_after_caret_move):
            result, lines, recognized = gui.move_to_document_top(
                interaction, None, None, None, "helper", 123, "window",
                Path(__file__).parent, "test-top", 1.0,
            )

        self.assertEqual(result["result"], "pass")
        self.assertEqual(lines, [1, 2])
        self.assertEqual(recognized, "LINE 001\nLINE 002")

    def assert_helper_failure_blocks_capture(self, failed_call):
        class RecordingInteraction:
            def __init__(self):
                self.calls = []

            def run_helper(self, helper, arguments, timeout):
                self.calls.append((helper, arguments, timeout))
                if len(self.calls) == failed_call:
                    return False, "", "expected helper failure"
                return True, "", None

        interaction = RecordingInteraction()
        with patch.object(gui, "capture_single") as capture:
            result, lines, recognized = gui.move_to_document_top(
                interaction, None, None, None, "helper", 123, "window",
                Path(__file__).parent, "test-top", 1.0,
            )

        self.assertEqual(result["result"], "blocked")
        self.assertEqual(result["reason"], "expected helper failure")
        self.assertIsNone(lines)
        self.assertEqual(recognized, "")
        capture.assert_not_called()

    def test_document_start_helper_failure_blocks_ocr_capture(self):
        self.assert_helper_failure_blocks_capture(failed_call=1)

    def test_caret_helper_failure_blocks_ocr_capture(self):
        self.assert_helper_failure_blocks_capture(failed_call=2)

    def test_failed_final_reset_preserves_ocr_evidence(self):
        top = gui.step(
            "direction-calibration-top", "pass", visible_lines=[1],
            recognized_text="LINE 001",
        )
        failed_reset = gui.step(
            "direction-calibration-final-reset", "blocked",
            "文書先頭への移動後に先頭行を確認できない",
            visible_lines=[2], recognized_text="LINE 002",
        )
        with (
            patch.object(gui, "move_to_document_top", side_effect=[
                (top, [1], "LINE 001"),
                (failed_reset, [2], "LINE 002"),
            ]),
            patch.object(gui, "capture_frames", return_value=(frames([2], [0]), None, None)),
        ):
            sign, result = gui.calibrate_scroll_direction(
                object(), None, None, None, None, 1, "window", Path(__file__).parent, 1.0,
            )

        self.assertIsNone(sign)
        self.assertEqual(result["result"], "blocked")
        self.assertEqual(result["reset_visible_lines"], [2])
        self.assertEqual(result["reset_recognized_text"], "LINE 002")
        self.assertEqual(result["reset_step"]["visible_lines"], [2])

    def test_behavior_inputs_use_one_calibrated_sign(self):
        source = MODULE_PATH.read_text(encoding="utf-8")
        body = source.split("def run_scroll_behavior_checks(", 1)[1].split("\ndef run_focused_scenario", 1)[0]
        self.assertIn('"lines", downward_sign * 8', body)
        self.assertIn('reverse_delta=downward_sign * -12', body)
        self.assertIn('"lines", -downward_sign * 1000', body)
        self.assertIn('"lines", downward_sign * 1200', body)


class VisionWarmupFailsClosedTests(unittest.TestCase):
    def test_visible_line_numbers_surfaces_raw_vision_errors_through_fail(self):
        source = SWIFT_HELPER_PATH.read_text(encoding="utf-8")
        body = source.split("func visibleLineNumbers(", 1)[1].split("\n}", 1)[0]
        self.assertIn('fail("could not inspect captured scroll frame: \\(error)")', body)

    def test_accurate_recognition_comment_does_not_claim_runtime_proof(self):
        source = SWIFT_HELPER_PATH.read_text(encoding="utf-8")
        body = source.split("func visibleLineNumbers(", 1)[1].split("\n}", 1)[0]
        self.assertIn("not proof this configuration is crash-free during an", body)


class WindowServerDisplayCaptureContractTests(unittest.TestCase):
    def test_display_time_and_pixels_come_from_the_same_complete_stream_sample(self):
        source = SWIFT_HELPER_PATH.read_text(encoding="utf-8")
        callback = source.split(
            "func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer,",
            1,
        )[1].split("\n    func stream(_ stream: SCStream, didStopWithError", 1)[0]
        self.assertIn("attachments[SCStreamFrameInfo.status]", callback)
        self.assertIn("statusValue as? Int", callback)
        self.assertIn("SCFrameStatus(rawValue: statusRawValue)", callback)
        self.assertIn("status == .complete", callback)
        self.assertIn("attachments[SCStreamFrameInfo.displayTime] as? UInt64", callback)
        self.assertIn("CMSampleBufferGetImageBuffer(sampleBuffer)", callback)
        self.assertIn("CIImage(cvPixelBuffer: pixelBuffer)", callback)
        self.assertIn("displayTicks: displayTicks", callback)

    def test_measurement_uses_stream_response_without_repurposing_product_presentation(self):
        source = SWIFT_HELPER_PATH.read_text(encoding="utf-8")
        measure = source.split("func wheelMeasure(", 1)[1].split("\n}", 1)[0]
        self.assertLess(measure.index("displayCapture.start()"), measure.index("postScrollTicks(pid, unit, delta)"))
        self.assertLess(measure.index("focus(pid)"), measure.index("postScrollTicks(pid, unit, delta)"))
        self.assertIn("displayCapture.markEventPosted(eventPostedTicks)", measure)
        self.assertIn('print(prefix + "image_source=same_CMSampleBuffer")', measure)
        self.assertIn('product_frame_presented_ticks=unavailable', measure)
        self.assertEqual(gui.PROCEDURE_VERSION, "hosted-scroll-inertia/14")


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
