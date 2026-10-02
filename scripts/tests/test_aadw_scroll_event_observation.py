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
    product_presented_ticks=4_000_000,
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
            f"product_frame_presented_ticks={product_presented_ticks}",
            f"product_mach_timebase_numer={product_ratio[0]}",
            f"product_mach_timebase_denom={product_ratio[1]}",
        ]
    else:
        lines += [
            "product_scroll_receipt_ticks=unavailable",
            "product_frame_presented_ticks=unavailable",
            "product_mach_timebase_numer=unavailable",
            "product_mach_timebase_denom=unavailable",
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
        self.assertAlmostEqual(result["stages_ms"]["event_post_ms"], 0.0)
        self.assertLess(result["stages_ms"]["scroll_receipt_ms"], result["stages_ms"]["frame_presented_ms"])
        self.assertLess(
            result["stages_ms"]["frame_presented_ms"],
            result["stages_ms"]["frames_ms"][0]["capture_started_ms"],
        )

    def test_disordered_measurement_is_reported_without_a_product_verdict(self):
        # Frame-presented ticks before the scroll-receipt ticks: an internally
        # inconsistent record, not evidence the product itself is slow or broken.
        record = scroll_event_observation.parse_wheel_measure_output(
            helper_output(product_receipt_ticks=5_000_000, product_presented_ticks=1_000_000),
            expected_frames=1,
        )
        result = scroll_event_observation.assess_wheel_measurement(record)
        self.assertEqual(result["observation"], "observed_disordered")
        self.assertTrue(result["clock_consistent"])
        self.assertIsNotNone(result["reason"])
        # The raw, unjudged mach-derived values are preserved alongside the
        # disordered observation rather than discarded.
        self.assertIn("scroll_receipt_ms", result["stages_ms"])

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


if __name__ == "__main__":
    unittest.main()
