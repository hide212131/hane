import datetime as dt
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "hosted_date_badge_gui.py"
spec = importlib.util.spec_from_file_location("hosted_date_badge_gui", SCRIPT)
assert spec is not None and spec.loader is not None
mod = importlib.util.module_from_spec(spec)
sys.modules["hosted_date_badge_gui"] = mod
spec.loader.exec_module(mod)


class HostedDateBadgeGuiTests(unittest.TestCase):
    def test_procedure_identity_is_focused(self):
        self.assertEqual(mod.PROCEDURE_VERSION, "hosted-date-badge/1")
        self.assertEqual(mod.VERIFICATION_KIND, "sidebar_date_badge_focused")

    def test_relative_label_shapes(self):
        today = dt.date(2026, 9, 17)
        self.assertEqual(mod.relative_label(today, today), "本日")
        self.assertEqual(mod.relative_label(dt.date(2026, 9, 1), today), "1日(火)")
        self.assertEqual(mod.relative_label(dt.date(2026, 10, 3), today), "10/3(土)")
        self.assertEqual(mod.relative_label(dt.date(2025, 10, 3), today), "2025/10/3(金)")

    def test_nearest_same_row_rejects_far_badge(self):
        text = {"bounding_box": {"minX": 0.10, "maxX": 0.20, "minY": 0.50, "maxY": 0.52}}
        near = {"bounding_box": {"minX": 0.25, "maxX": 0.30, "minY": 0.49, "maxY": 0.51}}
        far = {"bounding_box": {"minX": 0.25, "maxX": 0.30, "minY": 0.20, "maxY": 0.22}}
        self.assertIs(mod.nearest_same_row(text, [far, near]), near)
        self.assertIsNone(mod.nearest_same_row(text, [far]))

    def test_fixture_names_cover_required_shapes(self):
        with tempfile.TemporaryDirectory() as tmp:
            before, cases = mod.make_fixtures(Path(tmp) / "work-folder")
            names = {case["name"] for case in cases}
            self.assertEqual(
                names,
                {
                    "date_at_start",
                    "date_in_middle",
                    "date_at_end",
                    "one_digit_month_day",
                    "long_name_keeps_badge_visible",
                },
            )
            self.assertEqual(before, sorted(case["filename"] for case in cases))
            self.assertTrue(all((Path(tmp) / "work-folder" / name).is_file() for name in before))


if __name__ == "__main__":
    unittest.main()
