import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "scroll_event_observation", ROOT / "aadw_scroll_event_observation.py"
)
scroll_event_observation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scroll_event_observation)


NUMER, DENOM = 125, 3  # an arbitrary non-trivial mach timebase ratio, as on real Apple Silicon


def helper_output(
    *,
    event_post_ticks=0,
    product_receipt_ticks=1_000_000,
    product_paint_ticks=4_000_000,
    frame_started_ticks=8_000_000,
    frame_completed_ticks=8_200_000,
    event_route="cghidEventTap",
    helper_ratio=(NUMER, DENOM),
    product_ratio=(NUMER, DENOM),
    include_product=True,
    include_frame=True,
) -> str:
    lines = [
        f"event_route={event_route}",
        f"event_post_ticks={event_post_ticks}",
        f"mach_timebase_numer={helper_ratio[0]}",
        f"mach_timebase_denom={helper_ratio[1]}",
    ]
    if include_product:
        lines += [
            f"product_scroll_receipt_ticks={product_receipt_ticks}",
            f"product_frame_paint_ticks={product_paint_ticks}",
            # Hane always reports true compositor presentation as
            # unavailable; this fixture mirrors that so parsing/assessment
            # never has to special-case it away.
            "product_frame_presented_ticks=unavailable",
            f"product_mach_timebase_numer={product_ratio[0]}",
            f"product_mach_timebase_denom={product_ratio[1]}",
        ]
    else:
        lines += [
            "product_scroll_receipt_ticks=unavailable",
            "product_frame_paint_ticks=unavailable",
            "product_frame_presented_ticks=unavailable",
            "product_mach_timebase_numer=unavailable",
            "product_mach_timebase_denom=unavailable",
        ]
    response_display_ticks = event_post_ticks + 8_400_000
    response_callback_ticks = event_post_ticks + 9_000_000
    lines += [
        "display_response_count=1",
        "display_response_collection_valid=true",
        "display_response_00_sample_id=1",
        "display_response_00_frame_status=complete",
        "display_response_00_timestamp_source=SCStreamFrameInfo.displayTime",
        "display_response_00_image_source=same_CMSampleBuffer",
        f"display_response_00_display_time_ticks={response_display_ticks}",
        f"display_response_00_callback_received_ticks={response_callback_ticks}",
        f"display_response_00_image_ready_ticks={response_callback_ticks + 100_000}",
        f"display_response_00_artifact_written_ticks={response_callback_ticks + 200_000}",
        "display_response_00_image_path=/tmp/display-response-00.png",
    ]
    if include_frame:
        lines += [
            f"frame_00_capture_started_ticks={frame_started_ticks}",
            f"frame_00_capture_completed_ticks={frame_completed_ticks}",
        ]
    return "\n".join(lines) + "\n"


class ParseWheelMeasureOutputTests(unittest.TestCase):
    def test_parses_all_fields(self):
        record = scroll_event_observation.parse_wheel_measure_output(helper_output(), expected_frames=1)
        self.assertEqual(record["event_route"], "cghidEventTap")
        self.assertEqual(record["event_post_ticks"], 0)
        self.assertEqual(record["product_scroll_receipt_ticks"], 1_000_000)
        self.assertEqual(record["frames"][0]["capture_started_ticks"], 8_000_000)
        self.assertEqual(record["display_response_count"], 1)
        self.assertEqual(record["display_responses"][0]["sample_id"], 1)

    def test_unavailable_product_fields_parse_as_none(self):
        record = scroll_event_observation.parse_wheel_measure_output(
            helper_output(include_product=False), expected_frames=1
        )
        self.assertIsNone(record["product_scroll_receipt_ticks"])
        self.assertIsNone(record["product_mach_timebase_numer"])


class AssessWheelMeasurementTests(unittest.TestCase):
    def test_normal_well_ordered_measurement_is_observed(self):
        record = scroll_event_observation.parse_wheel_measure_output(helper_output(), expected_frames=1)
        result = scroll_event_observation.assess_wheel_measurement(record)
        self.assertEqual(result["observation"], "observed_ordered")
        self.assertTrue(result["clock_consistent"])
        self.assertIsNone(result["reason"])
        self.assertEqual(result["presentation_observation"], "unavailable")
        self.assertEqual(result["window_server_display_observation"], "observed")
        self.assertAlmostEqual(result["stages_ms"]["event_post_ms"], 0.0)
        self.assertLess(result["stages_ms"]["scroll_receipt_ms"], result["stages_ms"]["frame_paint_ms"])
        self.assertLess(
            result["stages_ms"]["frame_paint_ms"],
            result["stages_ms"]["frames_ms"][0]["capture_started_ms"],
        )

    def test_disordered_measurement_is_reported_without_a_product_verdict(self):
        # Frame-paint ticks before the scroll-receipt ticks: an internally
        # inconsistent record, not evidence the product itself is slow or broken.
        record = scroll_event_observation.parse_wheel_measure_output(
            helper_output(product_receipt_ticks=5_000_000, product_paint_ticks=1_000_000),
            expected_frames=1,
        )
        result = scroll_event_observation.assess_wheel_measurement(record)
        self.assertEqual(result["observation"], "observed_disordered")
        self.assertTrue(result["clock_consistent"])
        self.assertIsNotNone(result["reason"])
        self.assertEqual(result["presentation_observation"], "unavailable")
        # The raw, unjudged mach-derived values are preserved alongside the
        # disordered observation rather than discarded.
        self.assertIn("scroll_receipt_ms", result["stages_ms"])

    def test_an_early_pre_paint_frame_does_not_cause_a_disordered_observation(self):
        # Reproduces Issue #427's observed gap: frame 0 is scheduled at 0ms
        # delay and its capture legitimately starts before Hane's own paint
        # (1.565ms vs. a 34.215ms paint), while frame 1 starts well after
        # paint. The causal chain itself (event post -> receipt -> paint) is
        # still in order, so this must be `observed_ordered`, not
        # `observed_disordered` from comparing frame 0 against paint.
        output = "\n".join([
            "event_route=cghidEventTap",
            "event_post_ticks=0",
            "mach_timebase_numer=1",
            "mach_timebase_denom=1",
            "product_scroll_receipt_ticks=2534000",
            "product_frame_paint_ticks=34215000",
            "product_frame_presented_ticks=unavailable",
            "product_mach_timebase_numer=1",
            "product_mach_timebase_denom=1",
            "display_response_count=1",
            "display_response_collection_valid=true",
            "display_response_00_sample_id=1",
            "display_response_00_frame_status=complete",
            "display_response_00_timestamp_source=SCStreamFrameInfo.displayTime",
            "display_response_00_image_source=same_CMSampleBuffer",
            "display_response_00_display_time_ticks=80000000",
            "display_response_00_callback_received_ticks=81000000",
            "display_response_00_image_ready_ticks=82000000",
            "display_response_00_artifact_written_ticks=83000000",
            "display_response_00_image_path=/tmp/display-response-00.png",
            "frame_00_capture_started_ticks=1565000",
            "frame_00_capture_completed_ticks=3000000",
            "frame_01_capture_started_ticks=59002000",
            "frame_01_capture_completed_ticks=72418000",
        ]) + "\n"
        record = scroll_event_observation.parse_wheel_measure_output(output, expected_frames=2)
        result = scroll_event_observation.assess_wheel_measurement(record)
        self.assertEqual(result["observation"], "observed_ordered")
        self.assertTrue(result["clock_consistent"])
        self.assertIsNone(result["reason"])
        frames_ms = result["stages_ms"]["frames_ms"]
        self.assertTrue(frames_ms[0]["captured_before_paint"])
        self.assertFalse(frames_ms[1]["captured_before_paint"])
        # Raw timestamps are preserved even for the pre-paint frame.
        self.assertAlmostEqual(frames_ms[0]["capture_started_ms"], 1.565)
        self.assertAlmostEqual(frames_ms[1]["capture_started_ms"], 59.002)

    def test_presentation_is_always_reported_unavailable_when_measurement_is_unavailable(self):
        record = scroll_event_observation.parse_wheel_measure_output(
            helper_output(include_product=False), expected_frames=1
        )
        result = scroll_event_observation.assess_wheel_measurement(record)
        self.assertEqual(result["observation"], "unavailable")
        self.assertEqual(result["presentation_observation"], "unavailable")

    def test_missing_product_side_is_measurement_unavailable(self):
        record = scroll_event_observation.parse_wheel_measure_output(
            helper_output(include_product=False), expected_frames=1
        )
        result = scroll_event_observation.assess_wheel_measurement(record)
        self.assertEqual(result["observation"], "unavailable")
        self.assertEqual(result["stages_ms"], {})
        self.assertFalse(result["clock_consistent"])

    def test_mismatched_timebase_ratio_is_measurement_unavailable(self):
        record = scroll_event_observation.parse_wheel_measure_output(
            helper_output(product_ratio=(1, 1)), expected_frames=1
        )
        result = scroll_event_observation.assess_wheel_measurement(record)
        self.assertEqual(result["observation"], "unavailable")
        self.assertFalse(result["clock_consistent"])

    def test_wrong_event_route_is_measurement_unavailable(self):
        record = scroll_event_observation.parse_wheel_measure_output(
            helper_output(event_route="postToPid"), expected_frames=1
        )
        result = scroll_event_observation.assess_wheel_measurement(record)
        self.assertEqual(result["observation"], "unavailable")

    def test_missing_frame_capture_is_measurement_unavailable(self):
        record = scroll_event_observation.parse_wheel_measure_output(
            helper_output(include_frame=False), expected_frames=1
        )
        result = scroll_event_observation.assess_wheel_measurement(record)
        self.assertEqual(result["observation"], "unavailable")

    def test_missing_event_post_is_measurement_unavailable(self):
        output = "\n".join(
            line for line in helper_output().splitlines() if not line.startswith("event_post_ticks=")
        ) + "\n"
        record = scroll_event_observation.parse_wheel_measure_output(output, expected_frames=1)
        result = scroll_event_observation.assess_wheel_measurement(record)
        self.assertEqual(result["observation"], "unavailable")


class AssessWheelCaptureTimingTests(unittest.TestCase):
    def setUp(self):
        """Provide one ordered event, receipt, and paint timing sample."""
        self.output = "\n".join([
            "event_route=cghidEventTap",
            "event_post_ticks=1000000",
            f"mach_timebase_numer={NUMER}",
            f"mach_timebase_denom={DENOM}",
            "product_scroll_receipt_ticks=1100000",
            "product_frame_paint_ticks=1400000",
            "product_frame_presented_ticks=unavailable",
            f"product_mach_timebase_numer={NUMER}",
            f"product_mach_timebase_denom={DENOM}",
        ]) + "\n"

    def test_observes_event_receipt_and_paint_on_regular_capture_path(self):
        """Report the three capture-path stages on the shared clock."""
        record = scroll_event_observation.parse_wheel_capture_timing_output(self.output)
        result = scroll_event_observation.assess_wheel_capture_timing(record)
        self.assertEqual(result["observation"], "observed_ordered")
        self.assertTrue(result["clock_consistent"])
        self.assertAlmostEqual(result["stages_ms"]["event_to_receipt_ms"], 4.166666666666667)
        self.assertAlmostEqual(result["stages_ms"]["receipt_to_paint_ms"], 12.5)
        self.assertEqual(result["presentation_observation"], "unavailable")

    def test_missing_product_receipt_stays_unavailable(self):
        """Keep missing application timing unavailable instead of guessing."""
        output = self.output.replace(
            "product_scroll_receipt_ticks=1100000", "product_scroll_receipt_ticks=unavailable")
        record = scroll_event_observation.parse_wheel_capture_timing_output(output)
        result = scroll_event_observation.assess_wheel_capture_timing(record)
        self.assertEqual(result["observation"], "unavailable")
        self.assertFalse(result["clock_consistent"])
        self.assertEqual(result["presentation_observation"], "unavailable")

    def test_out_of_order_product_ticks_are_not_reported_as_ordered(self):
        """Flag a paint timestamp that precedes the recorded receipt."""
        output = self.output.replace(
            "product_frame_paint_ticks=1400000", "product_frame_paint_ticks=900000")
        record = scroll_event_observation.parse_wheel_capture_timing_output(output)
        result = scroll_event_observation.assess_wheel_capture_timing(record)
        self.assertEqual(result["observation"], "observed_disordered")
        self.assertTrue(result["clock_consistent"])
        self.assertIsNotNone(result["reason"])


if __name__ == "__main__":
    unittest.main()
